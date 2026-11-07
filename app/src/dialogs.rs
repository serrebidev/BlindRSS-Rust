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
fn name_row(parent: &impl WxWidget, sizer: &BoxSizer, label: &str, value: &str) -> TextCtrl {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(parent).with_label(label).build();
    let ctrl = TextCtrl::builder(parent).build();
    ctrl.set_value(value);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    ctrl
}

/// A labeled choice row (label + Choice); returns the choice.
/// Display strings pass through tr(); selections are read by
/// index only.
fn choice_row(
    parent: &impl WxWidget,
    sizer: &BoxSizer,
    label: &str,
    items: &[String],
    selected: usize,
) -> Choice {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(parent)
        .with_label(&crate::tr(label))
        .build();
    let choice = Choice::builder(parent).build();
    for item in items {
        choice.append(&crate::tr(item));
    }
    if !items.is_empty() {
        choice.set_selection(selected.min(items.len() - 1) as u32);
    }
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&choice, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    choice
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
/// The guide topic for a dialog kind: a Python dialog class
/// name when DIALOG_TOPICS has it, else a guide topic id used
/// directly.
fn dialog_topic(kind: &str) -> &'static str {
    blindrss_core::help_topics::topic_for_dialog(kind).unwrap_or_else(|| {
        blindrss_core::help_topics::TOPICS
            .iter()
            .find(|(id, _)| *id == kind)
            .map(|(id, _)| *id)
            .unwrap_or(blindrss_core::help_topics::DEFAULT_TOPIC)
    })
}

/// F1 inside a dialog opens the guide at the section documenting
/// that dialog (Python's DIALOG_TOPICS; the kind strings are the
/// Python class names, reused verbatim). The key is
/// binding-aware (Audit 8 S06, Python's HelpKeyFilter): help
/// opens only when the press is the accelerator bound to User
/// Guide, or F1 while F1 itself is unbound (the platform help
/// key). When the user has given F1 to another command, the
/// press is left alone — no help intercept.
pub fn bind_f1(frame: &Frame, dialog: &Dialog, kind: &'static str) {
    bind_f1_impl(frame, dialog, kind, None);
}

/// bind_f1 for a dialog built around a Notebook: the topic
/// resolves through the CURRENT page first (Python's per-page
/// `_help_topic` attributes), named by its English page title,
/// falling back to the dialog topic.
pub fn bind_f1_notebook(
    frame: &Frame,
    dialog: &Dialog,
    kind: &'static str,
    notebook: &Notebook,
    page_titles: &'static [&'static str],
) {
    bind_f1_impl(frame, dialog, kind, Some((*notebook, page_titles)));
}

fn bind_f1_impl(
    frame: &Frame,
    dialog: &Dialog,
    kind: &'static str,
    notebook: Option<(Notebook, &'static [&'static str])>,
) {
    let parent = *frame;
    dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
        let code = ev.get_key_code().unwrap_or(0);
        let mods = blindrss_core::shortcuts::KeyModifiers {
            ctrl: ev.control_down(),
            alt: ev.alt_down(),
            shift: ev.shift_down(),
            cmd: ev.meta_down(),
        };
        let Some(accel) = blindrss_core::shortcuts::event_accel(code, mods, true) else {
            ev.skip(true);
            return;
        };
        let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
        let map = blindrss_core::shortcuts::accel_map(
            &blindrss_core::shortcuts::overrides_from_config(&cfg),
        );
        if blindrss_core::shortcuts::help_key_action(&accel, &map)
            != blindrss_core::shortcuts::HelpKeyAction::ContextHelp
        {
            ev.skip(true);
            return;
        }
        let topic = notebook
            .and_then(|(nb, titles)| {
                let idx = nb.selection().max(0) as usize;
                titles
                    .get(idx)
                    .and_then(|t| blindrss_core::help_topics::topic_for_notebook_page(kind, t))
            })
            .unwrap_or_else(|| dialog_topic(kind));
        crate::help::show_help_topic(&parent, &crate::ui_language(), Some(topic));
        ev.skip(false);
    });
}

pub fn show_filter_rules(frame: &Frame, db: &Db) {
    let dialog = Dialog::builder(frame, "Filter Rules").build();
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
    use blindrss_core::feed_settings as fs;
    let settings0 = db.feed_settings(&feed.id).unwrap_or(Value::Null);
    let dialog = Dialog::builder(frame, "Feed Properties").build();
    dialog.set_escape_id(5101);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    // Python's dialog is a notebook: General + List Headers.
    // The existing General controls keep their order; the
    // behavioral overrides (Audit 8 G08) are appended after
    // them in Python's relative order, per the append-only
    // rule.
    let notebook = Notebook::builder(&dialog).build();
    let general = ScrolledWindow::builder(&notebook).build();
    general.set_scroll_rate(0, 12);
    let gsizer = BoxSizer::builder(Orientation::Vertical).build();
    notebook.add_page(&general, &crate::tr("General"), true, None);
    let headers_page = ScrolledWindow::builder(&notebook).build();
    headers_page.set_scroll_rate(0, 12);
    let hsizer = BoxSizer::builder(Orientation::Vertical).build();
    notebook.add_page(&headers_page, &crate::tr("List Headers"), false, None);
    sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 2);
    // F1 resolves through the current page (Python's per-page
    // help topics for this dialog exist in core's table).
    bind_f1_notebook(
        frame,
        &dialog,
        "FeedPropertiesDialog",
        &notebook,
        &["General", "List Headers"],
    );

    let title_ctrl = name_row(&general, &gsizer, "Title:", &feed.title);
    title_ctrl.set_name("Feed title");
    let url_ctrl = name_row(&general, &gsizer, "Feed URL:", &feed.url);
    url_ctrl.set_name("Feed URL");
    let cat0 = feed.category.clone().unwrap_or_default();
    let cat_ctrl = name_row(&general, &gsizer, "Category (blank = none):", &cat0);
    // Choose... opens the category picker (existing categories
    // plus inline creation) and fills the field.
    let choose_cat_btn = Button::builder(&general)
        .with_label(&crate::tr("Choose Category..."))
        .build();
    gsizer.add(&choose_cat_btn, 0, SizerFlag::All, 4);
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
    // issue #75): in Python this is the feed-document encoding;
    // it also remains the extraction fallback (see the save).
    let encoding0 = fs::text_value(&settings0, "encoding");
    let enc_ctrl = name_row(
        &general,
        &gsizer,
        "Encoding override (blank = auto-detect):",
        &encoding0,
    );
    enc_ctrl.set_name("Feed encoding");
    let reset_btn = Button::builder(&general)
        .with_label(&crate::tr("Reset Title to Feed's Own"))
        .build();
    gsizer.add(&reset_btn, 0, SizerFlag::All, 4);

    // ---- Behavioral overrides (Audit 8 G08; Python
    // gui/dialogs.py FeedPropertiesDialog General page). Every
    // value round-trips through one merged feed_settings patch
    // (core feed_settings::build_patch) so keys this dialog
    // does not show are never erased.
    let interval_items: Vec<String> = fs::REFRESH_INTERVAL_CHOICES
        .iter()
        .map(|(_, label)| label.to_string())
        .collect();
    let interval_choice = choice_row(
        &general,
        &gsizer,
        "Refresh interval:",
        &interval_items,
        fs::refresh_interval_index(&settings0),
    );
    // Custom request headers, one "Name: Value" per line.
    let headers_label = StaticText::builder(&general)
        .with_label(&crate::tr("Custom request headers:"))
        .build();
    gsizer.add(&headers_label, 0, SizerFlag::All, 4);
    let headers_ctrl = TextCtrl::builder(&general)
        .with_style(TextCtrlStyle::MultiLine)
        .build();
    headers_ctrl.set_value(&fs::headers_text(&settings0));
    crate::msaa_names::set_name(&headers_ctrl, &crate::tr("Custom request headers"));
    gsizer.add(&headers_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let timeout_ctrl = name_row(
        &general,
        &gsizer,
        "Request timeout seconds (blank = default):",
        &fs::timeout_text(&settings0),
    );
    timeout_ctrl.set_name("Request timeout seconds");
    let impersonate_items: Vec<String> = fs::IMPERSONATE_LABELS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let impersonate_choice = choice_row(
        &general,
        &gsizer,
        "Browser impersonation:",
        &impersonate_items,
        fs::IMPERSONATE_VALUES
            .iter()
            .position(|v| *v == fs::impersonate_value(&settings0))
            .unwrap_or(0),
    );
    let proxy_ctrl = name_row(
        &general,
        &gsizer,
        "Proxy URL (blank = none):",
        &fs::text_value(&settings0, "proxy"),
    );
    proxy_ctrl.set_name("Proxy URL");
    let fulltext_ctrl = name_row(
        &general,
        &gsizer,
        "Full text extraction encoding (blank = auto-detect):",
        &fs::text_value(&settings0, "fulltext_encoding"),
    );
    fulltext_ctrl.set_name("Full text extraction encoding");
    // Delete behavior lives in its own feeds column, not in
    // feed_settings (Python stores it the same way).
    let (delete_sel0, delete_target0) = match db
        .feed_delete_behavior(&feed.id)
        .ok()
        .flatten()
        .as_deref()
        .map(blindrss_core::rules::parse_delete_behavior)
    {
        None => (0usize, String::new()),
        Some(blindrss_core::rules::DeleteBehavior::Deleted) => (1, String::new()),
        Some(blindrss_core::rules::DeleteBehavior::Purge) => (2, String::new()),
        Some(blindrss_core::rules::DeleteBehavior::Category(path)) => (3, path),
    };
    let delete_items: Vec<String> = [
        "Use global setting",
        "Move it to Deleted Articles",
        "Remove it permanently",
        "Move it to a category",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let delete_choice = choice_row(
        &general,
        &gsizer,
        "When articles are deleted:",
        &delete_items,
        delete_sel0,
    );
    let delete_target_ctrl = name_row(
        &general,
        &gsizer,
        "Delete target category (full path):",
        &delete_target0,
    );
    delete_target_ctrl.enable(delete_sel0 == 3);
    delete_choice.on_selection_changed(move |_| {
        delete_target_ctrl.enable(delete_choice.get_selection() == Some(3));
    });
    general.set_sizer(gsizer, true);

    // ---- List Headers page: the per-feed column editor
    // (Audit 8 S03). "Use the global column layout" checked =
    // inherit (no per-feed layout); unchecking edits the feed's
    // own layout, starting from the inherited one.
    let feed_layout0 = blindrss_core::article_columns::feed_layout_from_settings(&settings0);
    let inherit0 = feed_layout0.is_none();
    let global_layout = blindrss_core::article_columns::layout_from_config(
        &Config::load(&crate::config_path()).unwrap_or_default(),
    );
    let inherit_cb = CheckBox::builder(&headers_page)
        .with_label(&crate::tr("Use the global column layout"))
        .build();
    inherit_cb.set_value(inherit0);
    hsizer.add(&inherit_cb, 0, SizerFlag::All, 4);
    let editor = ColumnEditor::build(
        &headers_page,
        &hsizer,
        feed_layout0.unwrap_or(global_layout),
        true,
    );
    editor.set_enabled(!inherit0);
    {
        let editor = editor.clone();
        inherit_cb.on_toggled(move |_| {
            editor.set_enabled(!inherit_cb.is_checked());
        });
    }
    headers_page.set_sizer(hsizer, true);

    // OK validates the encoding fields before anything is
    // saved (Python refuses the dialog with its codec error).
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
    {
        let d = dialog;
        ok_btn.on_click(move |_| {
            for ctrl in [enc_ctrl, fulltext_ctrl] {
                let value = ctrl.get_value();
                if !fs::valid_encoding(value.trim()) {
                    let md = MessageDialog::builder(
                        &d,
                        &format!(
                            "Unknown text encoding: {}. Use a Python codec name such as utf-8, windows-1251 or koi8-r, or leave the field blank for automatic detection.",
                            value.trim()
                        ),
                        "Feed Properties",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
                    .build();
                    md.show_modal();
                    md.destroy();
                    return;
                }
            }
            d.end_modal(5100);
        });
    }
    {
        let d = dialog;
        cancel_btn.on_click(move |_| d.end_modal(5101));
    }
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 520));
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
        // One merged patch (Audit 8 G08): set_feed_settings
        // merges top-level keys, so settings this dialog does
        // not show survive; null values mean "inherit".
        let columns_patch = if inherit_cb.is_checked() {
            None
        } else {
            Some(blindrss_core::article_columns::layout_to_json(
                &editor.layout(),
            ))
        };
        let patch = fs::build_patch(
            &headers_ctrl.get_value(),
            &timeout_ctrl.get_value(),
            fs::IMPERSONATE_VALUES[impersonate_choice.get_selection().unwrap_or(0) as usize],
            &proxy_ctrl.get_value(),
            &enc_ctrl.get_value(),
            &fulltext_ctrl.get_value(),
            interval_choice.get_selection().unwrap_or(0) as usize,
            columns_patch,
        );
        let _ = db.set_feed_settings(&feed_id, &patch);
        let delete_encoded: Option<String> = match delete_choice.get_selection().unwrap_or(0) {
            1 => Some("deleted".to_string()),
            2 => Some("purge".to_string()),
            3 => {
                let target = delete_target_ctrl
                    .get_value()
                    .split('/')
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(blindrss_core::db::CATEGORY_PATH_SEP);
                if target.is_empty() {
                    None
                } else {
                    Some(format!("category:{target}"))
                }
            }
            _ => None,
        };
        let _ = db.set_feed_delete_behavior(&feed_id, delete_encoded.as_deref());
        changed = true;
    }
    dialog.destroy();
    changed || *reset.borrow()
}

/// About BlindRSS (Python's AboutDialog content).
pub fn show_about(frame: &Frame) {
    let dialog = Dialog::builder(frame, "About BlindRSS").build();
    dialog.set_escape_id(5101);
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
/// Open Article dialog (Python's OpenArticleDialog, Audit 8
/// G13): the address (prefilled from the clipboard when it
/// holds a usable URL — the caller computes that) and the
/// starting reader. The checkbox only picks the *starting*
/// mode; the article window's own switch changes it without
/// fetching again. Returns (url as typed, rich) on OK; the
/// caller normalizes the URL.
pub fn show_open_article_dialog(
    frame: &Frame,
    initial_url: &str,
    rich_default: bool,
) -> Option<(String, bool)> {
    let dialog = Dialog::builder(frame, "Open Article").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "OpenArticleDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let intro = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Paste the address of an article or web page.\nBlindRSS reads it the way it reads a feed article, without the\npage's own layout, menus, or pop-ups.",
        ))
        .build();
    sizer.add(&intro, 0, SizerFlag::All, 4);
    let url_ctrl = name_row(&dialog, &sizer, "Article address:", initial_url);
    url_ctrl.set_name("Article address");
    let rich_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Open in &HTML view"))
        .build();
    rich_cb.set_value(rich_default);
    sizer.add(&rich_cb, 0, SizerFlag::All, 4);
    let note = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Cleared, the article opens as plain full text. Checked, it keeps headings, links, and embedded video.",
        ))
        .build();
    sizer.add(&note, 0, SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 300));
    let result = if dialog.show_modal() == 5100 {
        Some((url_ctrl.get_value(), rich_cb.is_checked()))
    } else {
        None
    };
    dialog.destroy();
    result
}

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
    dialog.set_escape_id(5101);
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

/// A reusable column-layout editor (Python's ColumnLayoutPanel):
/// list order is column order, checked is shown, Move Up / Move
/// Down reorder through core's move_key (Title pinned first),
/// Title stays visible. Used by the global Article Columns
/// dialog and by the per-feed editor in Feed Properties
/// (Audit 8 S03/S22). Display labels pass through tr(); the
/// stored keys never change (Audit 8 S10).
#[derive(Clone)]
struct ColumnEditor {
    state: Rc<RefCell<Vec<blindrss_core::article_columns::ColumnLayout>>>,
    list: CheckListBox,
    up_btn: Button,
    down_btn: Button,
    reset_btn: Button,
    sync_checks: Rc<dyn Fn()>,
}

impl ColumnEditor {
    fn build(
        parent: &impl WxWidget,
        sizer: &BoxSizer,
        initial: Vec<blindrss_core::article_columns::ColumnLayout>,
        with_help_text: bool,
    ) -> ColumnEditor {
        use blindrss_core::article_columns::{ColumnLayout, move_key, resolve_layout, set_visible};
        let state: Rc<RefCell<Vec<ColumnLayout>>> = Rc::new(RefCell::new(initial));
        if with_help_text {
            let help = StaticText::builder(parent)
                .with_label(&crate::tr(
                    "Columns (checked = shown). Use Move Up / Move Down to reorder:",
                ))
                .build();
            sizer.add(&help, 0, SizerFlag::All, 4);
        }
        let list = CheckListBox::builder(parent).build();
        crate::msaa_names::set_name(&list, &crate::tr("Columns"));
        sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
        let note = StaticText::builder(parent)
            .with_label(&crate::tr(
                "Title is always the first column and cannot be hidden.",
            ))
            .build();
        sizer.add(&note, 0, SizerFlag::All, 4);
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let up_btn = Button::builder(parent)
            .with_label(&crate::tr("Move Up"))
            .build();
        let down_btn = Button::builder(parent)
            .with_label(&crate::tr("Move Down"))
            .build();
        let reset_btn = Button::builder(parent)
            .with_label(&crate::tr("Reset All to Defaults"))
            .build();
        row.add(&up_btn, 0, SizerFlag::All, 4);
        row.add(&down_btn, 0, SizerFlag::All, 4);
        row.add(&reset_btn, 0, SizerFlag::All, 4);
        sizer.add_sizer(&row, 0, SizerFlag::All, 2);

        let rebuild: Rc<dyn Fn(Option<usize>)> = {
            let state = state.clone();
            Rc::new(move |select: Option<usize>| {
                let st = state.borrow();
                list.clear();
                for (i, c) in st.iter().enumerate() {
                    if c.key == blindrss_core::article_columns::PINNED_KEY {
                        list.append(&format!(
                            "{} {}",
                            crate::tr(&c.label),
                            crate::tr("(always first)")
                        ));
                    } else {
                        list.append(&crate::tr(&c.label));
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
                let key = state.borrow()[sel].key.clone();
                let mut st = state.borrow_mut();
                *st = move_key(&st, &key, -1);
                let new_sel = st.iter().position(|c| c.key == key).unwrap_or(sel);
                drop(st);
                rebuild(Some(new_sel));
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
                let key = state.borrow()[sel].key.clone();
                let mut st = state.borrow_mut();
                *st = move_key(&st, &key, 1);
                let new_sel = st.iter().position(|c| c.key == key).unwrap_or(sel);
                drop(st);
                rebuild(Some(new_sel));
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
        // Check toggles route through core's set_visible, so
        // Title can never be hidden even by a stray event.
        {
            let state = state.clone();
            let rebuild = rebuild.clone();
            list.on_toggled(move |e| {
                let Some(idx) = e.get_selection().map(|s| s as usize) else {
                    return;
                };
                let checked = list.is_checked(idx as u32);
                let mut st = state.borrow_mut();
                if let Some(key) = st.get(idx).map(|c| c.key.clone()) {
                    *st = set_visible(&st, &key, checked);
                }
                drop(st);
                rebuild(Some(idx));
            });
        }
        ColumnEditor {
            state,
            list,
            up_btn,
            down_btn,
            reset_btn,
            sync_checks,
        }
    }

    /// Enable or disable the whole editor (the Feed Properties
    /// inherit state disables it, as Python's panel does).
    fn set_enabled(&self, on: bool) {
        self.list.enable(on);
        self.up_btn.enable(on);
        self.down_btn.enable(on);
        self.reset_btn.enable(on);
    }

    /// The current layout with the widget's check states
    /// pulled in.
    fn layout(&self) -> Vec<blindrss_core::article_columns::ColumnLayout> {
        (self.sync_checks)();
        self.state.borrow().clone()
    }
}

/// Article column layout editor (Python's ArticleColumnsPanel
/// in Settings): list order is column order, checked is shown,
/// Move Up / Move Down reorder, Title stays first and visible.
/// Writes config "article_columns" on OK, in Python's stored
/// shape ({"key", "visible"} entries — no widths; Python's
/// layout carries none).
pub fn show_article_columns(frame: &Frame, config_path: &Path) {
    use blindrss_core::article_columns::{layout_from_config, layout_to_json};
    let cfg = Config::load(config_path).unwrap_or_default();
    let dialog = Dialog::builder(frame, "Article Columns").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "list-headers");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let editor = ColumnEditor::build(&dialog, &sizer, layout_from_config(&cfg), true);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_btn = Button::builder(&dialog)
        .with_label(&crate::tr("OK"))
        .build();
    let cancel_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    row.add(&ok_btn, 0, SizerFlag::All, 4);
    row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::All, 2);
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
    dialog.set_size(Size::new(480, 460));
    editor.list.set_focus();
    if dialog.show_modal() == 5100 && *saved.borrow() {
        let mut cfg = Config::load(config_path).unwrap_or_default();
        cfg.set_path("article_columns", layout_to_json(&editor.layout()));
        let _ = cfg.save(config_path);
    }
    dialog.destroy();
}

/// Add Feed (Python's AddFeedDialog, Audit 8 G18): a URL
/// field with a live compatibility hint and a category
/// combo. The hint updates as the user types (500 ms
/// debounce, worker thread for the heavier check — the
/// dialog never blocks): YouTube URLs are recognized
/// instantly and auto-select an existing YouTube category;
/// other URLs get Python's "OK: Supported by yt-dlp" when
/// the offline support check passes. `categories` are
/// identity paths with "" first (Uncategorized, displayed
/// as such). Returns (url, category identity — a typed
/// combo value is a new category path).
pub fn show_add_feed(
    frame: &Frame,
    categories: &[String],
    initial_url: &str,
) -> Option<(String, String)> {
    let dialog = Dialog::builder(frame, "Add Feed").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "AddFeedDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let url_ctrl = name_row(&dialog, &sizer, "Feed or Media URL:", initial_url);
    url_ctrl.set_name("Feed or Media URL");
    let hint = StaticText::builder(&dialog).with_label("").build();
    sizer.add(&hint, 0, SizerFlag::All, 4);
    let cat_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Category:"))
        .build();
    sizer.add(&cat_label, 0, SizerFlag::All, 4);
    let cat_ctrl = ComboBox::builder(&dialog).build();
    cat_ctrl.set_name("Category");
    for (i, cat) in categories.iter().enumerate() {
        cat_ctrl.append(if i == 0 { "Uncategorized" } else { cat });
    }
    // Python preselects an existing YouTube category.
    let yt_pos = categories.iter().position(|c| c == "YouTube").unwrap_or(0);
    cat_ctrl.set_selection(yt_pos as u32);
    sizer.add(&cat_ctrl, 0, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);

    // Async hint machinery: text changes record a
    // debounced evaluation; heavy checks run on a worker
    // and land through the same timer drain.
    let pending: Rc<RefCell<Option<(String, std::time::Instant)>>> = Rc::new(RefCell::new(None));
    let evaluated: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let (tx, rx) = std::sync::mpsc::channel::<(String, bool)>();
    let rx = Rc::new(rx);
    {
        let pending = pending.clone();
        url_ctrl.on_text_updated(move |_| {
            *pending.borrow_mut() = Some((
                url_ctrl.get_value().trim().to_string(),
                std::time::Instant::now() + std::time::Duration::from_millis(500),
            ));
        });
    }
    let timer = Timer::new(&dialog);
    {
        let pending = pending.clone();
        let evaluated = evaluated.clone();
        // Precomputed: the tick closure is 'static and
        // cannot borrow the categories slice.
        let youtube_pos = categories.iter().position(|c| c == "YouTube");
        timer.on_tick(move |_| {
            while let Ok((url, supported)) = rx.try_recv() {
                if url_ctrl.get_value().trim() == url {
                    let text = if supported {
                        crate::tr("OK: Supported by yt-dlp")
                    } else {
                        String::new()
                    };
                    hint.set_label(&text);
                }
            }
            let due = pending
                .borrow()
                .as_ref()
                .is_some_and(|(_, when)| std::time::Instant::now() >= *when);
            if !due {
                return;
            }
            let (url, _) = pending.borrow_mut().take().unwrap();
            if url == *evaluated.borrow() {
                return;
            }
            *evaluated.borrow_mut() = url.clone();
            if url.is_empty() {
                hint.set_label("");
                return;
            }
            let lower = url.to_lowercase();
            if lower.contains("youtube.com") || lower.contains("youtu.be") {
                hint.set_label(&crate::tr("OK: Recognized as YouTube source"));
                if let Some(pos) = youtube_pos {
                    cat_ctrl.set_selection(pos as u32);
                }
                return;
            }
            hint.set_label(&crate::tr("Checking compatibility..."));
            let tx = tx.clone();
            std::thread::spawn(move || {
                let supported = blindrss_core::discovery::ytdlp_supported_offline(&url);
                let _ = tx.send((url, supported));
            });
        });
    }
    timer.start(100, false);
    if !initial_url.trim().is_empty() {
        *pending.borrow_mut() = Some((initial_url.trim().to_string(), std::time::Instant::now()));
    }
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 300));
    url_ctrl.set_focus();
    let accepted = dialog.show_modal() == 5100;
    timer.stop();
    let result = if accepted {
        let url = url_ctrl.get_value().trim().to_string();
        let text = cat_ctrl.get_value().trim().to_string();
        let category = if text.is_empty() || text == "Uncategorized" {
            String::new()
        } else {
            text
        };
        Some((url, category))
    } else {
        None
    };
    dialog.destroy();
    result
}

/// Detected Feeds picker (Python's choice dialog in
/// `_present_detected_page_feeds`): rows read
/// "title (url)", or the bare URL when the detection
/// left the title empty.
pub fn show_detected_feed_choice(
    frame: &Frame,
    feeds: &[blindrss_core::discovery::DetectedFeed],
) -> Option<String> {
    let dialog = Dialog::builder(frame, "Detected Feeds").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "DetectFeedsDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Select a feed to subscribe to:"))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    list.set_name("Detected feeds");
    for feed in feeds {
        if feed.title.trim().is_empty() {
            list.append(&feed.url);
        } else {
            list.append(&format!("{} ({})", feed.title, feed.url));
        }
    }
    if !feeds.is_empty() {
        list.set_selection(0, true);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 380));
    let picked = if dialog.show_modal() == 5100 {
        list.get_selection()
            .and_then(|sel| feeds.get(sel as usize).map(|f| f.url.clone()))
    } else {
        None
    };
    dialog.destroy();
    picked
}

/// Video Search (Python's YtdlpGlobalSearchDialog): search
/// videos across yt-dlp sites, then Play / Subscribe / Copy
/// URL a result. Returns ("play" | "subscribe_best" |
/// "subscribe_native" | "subscribe_source", result) for
/// the caller to act on (Audit 8 F05: subscribing acts on
/// the result's retained native/source target, never the
/// bare video URL); Copy URL is handled in place.
/// Per-site searches run on their own threads (Python caps
/// concurrency at 8; the site list itself is the cap here)
/// and stream into the list through a timer drain, merged
/// by canonical key with Python's quality rule (a better
/// later duplicate replaces the stored copy). Load More
/// widens the per-site limit by 80 and re-runs, matching
/// Python's step.
pub fn show_video_search(
    frame: &Frame,
    config_path: &Path,
) -> Option<(&'static str, blindrss_core::videosearch::VideoResult)> {
    use blindrss_core::videosearch::{VideoResult, merge_result, search_site, searchable_sites};
    let cfg = Config::load(config_path).unwrap_or_default();
    let sites = searchable_sites(cfg.get_bool("enable_adult_search", false));
    let dialog = Dialog::builder(frame, "Video Search").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "YtdlpGlobalSearchDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let query_ctrl = name_row(&dialog, &sizer, "Search:", "");
    let opt_row = BoxSizer::builder(Orientation::Horizontal).build();
    let scope_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Search Sites:"))
        .build();
    opt_row.add(&scope_label, 0, SizerFlag::All, 4);
    let mut scope_items: Vec<String> = vec!["All sites".to_string()];
    scope_items.extend(sites.iter().map(|s| s.label.clone()));
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
    // Python's Subscribe (native) / Subscribe (source)
    // actions (Audit 8 F05), appended after the existing
    // buttons so no existing control moves.
    let sub_native_btn = mk("Subscribe (Native Feed)");
    let sub_source_btn = mk("Subscribe (Source)");
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
        sub_btn.on_click(move |_| pick("subscribe_best"));
    }
    {
        let pick = pick.clone();
        sub_native_btn.on_click(move |_| pick("subscribe_native"));
    }
    {
        let pick = pick.clone();
        sub_source_btn.on_click(move |_| pick("subscribe_source"));
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
                    let mut all = results.borrow_mut();
                    for r in found {
                        merge_result(&mut all, r);
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

/// Normalize a saved-search list the way Python's
/// `_normalize_persistent_searches` does: trim, drop empties,
/// dedupe case-insensitively keeping the first entry.
pub(crate) fn normalize_saved_searches(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let q = item.trim();
        if q.is_empty() || out.iter().any(|e: &String| e.eq_ignore_ascii_case(q)) {
            continue;
        }
        out.push(q.to_string());
    }
    out
}

/// Whether a query is already in the list (Python's
/// `_has_query`: trimmed, case-insensitive; an empty query
/// counts as present so it can never be added).
pub(crate) fn saved_search_present(list: &[String], query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    list.iter().any(|e| e.trim().eq_ignore_ascii_case(q))
}

/// Manage persistent searches (Python's
/// PersistentSearchDialog): edits are STAGED in a working
/// copy (Audit 8 G17) — Add/Remove change only the copy, OK
/// applies it to config "persistent_searches", Cancel
/// discards it. Returns a query to run when the user picks
/// Run on one (Run applies the staged edits too, as the
/// dialog closes with OK), None otherwise.
pub fn show_persistent_searches(frame: &Frame, config_path: &Path) -> Option<String> {
    let cfg = Config::load(config_path).unwrap_or_default();
    let stored: Vec<String> = cfg
        .raw()
        .get("persistent_searches")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let searches = Rc::new(RefCell::new(normalize_saved_searches(&stored)));
    let dialog = Dialog::builder(frame, "Configure Persistent Search").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "PersistentSearchDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Saved searches:"))
        .build();
    sizer.add(&list_label, 0, SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    crate::msaa_names::set_name(&list, &crate::tr("Saved searches"));
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
    let run_btn = mk(&crate::tr("Run Search"));
    let add_btn = mk(&crate::tr("Add..."));
    let remove_btn = mk(&crate::tr("Remove"));
    let ok_btn = mk(&crate::tr("OK"));
    let cancel_btn = mk(&crate::tr("Cancel"));
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
        let save = save.clone();
        run_btn.on_click(move |_| {
            if let Some(sel) = list2.get_selection() {
                let q = searches.borrow().get(sel as usize).cloned();
                *chosen.borrow_mut() = q;
            }
            // Run closes with OK: the staged edits apply.
            save();
            d.end_modal(5100);
        });
    }
    {
        let reload = reload.clone();
        let searches = searches.clone();
        let d = dialog;
        add_btn.on_click(move |_| {
            let entry = TextEntryDialog::builder(&d, "Search query:", "Add Search").build();
            if entry.show_modal() == 5100
                && let Some(q) = entry.get_value()
            {
                // Staged only (Audit 8 G17): OK applies,
                // Cancel discards. Duplicates are detected
                // case-insensitively, like Python.
                if !saved_search_present(&searches.borrow(), &q) {
                    searches.borrow_mut().push(q.trim().to_string());
                    reload();
                }
            }
            entry.destroy();
        });
    }
    {
        let reload = reload.clone();
        let searches = searches.clone();
        let list2 = list;
        remove_btn.on_click(move |_| {
            if let Some(sel) = list2.get_selection() {
                let mut s = searches.borrow_mut();
                if (sel as usize) < s.len() {
                    s.remove(sel as usize);
                }
                drop(s);
                reload();
            }
        });
    }
    {
        let d = dialog;
        let save = save.clone();
        ok_btn.on_click(move |_| {
            save();
            d.end_modal(5100);
        });
    }
    let d = dialog;
    cancel_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 420));
    dialog.show_modal();
    dialog.destroy();

    chosen.borrow().clone()
}

/// Heading line for the Feed Errors dialog (Python's wording).
pub(crate) fn feed_errors_heading(count: usize) -> String {
    if count == 0 {
        crate::tr("No feeds reported errors during their most recent update.")
    } else {
        format!(
            "{count} {}",
            crate::tr("feed(s) failed to update during their most recent attempt:")
        )
    }
}

/// The one-line form of a stored error for the report list:
/// whitespace collapsed, truncated at 220 characters with an
/// ellipsis (Python's list cell text).
pub(crate) fn feed_error_oneline(error: &str) -> String {
    let collapsed = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 220 {
        let cut: String = collapsed.chars().take(220).collect();
        format!("{cut}…")
    } else {
        collapsed
    }
}

/// The failures sentence in the detail text (Python's two
/// shapes).
pub(crate) fn feed_error_failures_line(count: i64) -> String {
    if count <= 1 {
        crate::tr("1 failed attempt so far (this may be a one-off glitch).")
    } else {
        format!(
            "{count} {}",
            crate::tr("consecutive failed attempt(s) (likely a persistent problem).")
        )
    }
}

/// A stored timestamp in the detail text: the app's shared
/// formatter, or Python's "Unknown" when none was recorded.
fn feed_error_ts(ts: Option<f64>) -> String {
    match ts {
        Some(t) if t > 0.0 => crate::fmt_ts(Some(t)),
        _ => crate::tr("Unknown"),
    }
}

/// The read-only detail text for one errored feed (Python's
/// exact layout; the error itself is preserved in full,
/// newlines included).
pub(crate) fn feed_error_detail(e: &blindrss_core::db::FeedError) -> String {
    format!(
        "Feed: {}\nURL: {}\nCategory: {}\nLast update attempt: {}\nLast successful update: {}\n{}\n\nError:\n{}",
        e.title,
        e.url,
        e.category.as_deref().unwrap_or(""),
        feed_error_ts(e.error_at),
        match e.success_at {
            Some(t) if t > 0.0 => crate::fmt_ts(Some(t)),
            _ => crate::tr("No successful update recorded."),
        },
        feed_error_failures_line(e.consecutive_failures),
        e.error,
    )
}

/// Feed Errors dialog (Audit 8 G10; Python's "Feeds with
/// Errors", gui/dialogs.py:4881): the error report with full
/// details, Copy, retry (button or Enter on the list) wired
/// to a real single-feed refresh, Feed Properties, and feed
/// removal. A resolved retry clears the stored error, exactly
/// like the main window's refresh drain. Returns true when
/// the feed set changed (removal or a properties edit), so
/// the caller can rebuild its views. Opens with the empty
/// state too — Python shows the explanatory dialog rather
/// than nothing.
pub fn show_feed_errors(frame: &Frame) -> bool {
    let changed = Rc::new(RefCell::new(false));
    let dialog = Dialog::builder(frame, "Feeds with Errors").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "FeedErrorsDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let heading = StaticText::builder(&dialog).with_label("").build();
    sizer.add(&heading, 0, SizerFlag::All, 4);
    let list = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report | ListCtrlStyle::SingleSel)
        .build();
    crate::msaa_names::set_name(&list, &crate::tr("Feeds with errors"));
    list.insert_column(0, &crate::tr("Feed"), ListColumnFormat::Left, 250);
    list.insert_column(1, &crate::tr("Last attempt"), ListColumnFormat::Left, 160);
    list.insert_column(2, &crate::tr("Failures"), ListColumnFormat::Left, 80);
    list.insert_column(3, &crate::tr("Error"), ListColumnFormat::Left, 320);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let detail_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Error details:"))
        .build();
    sizer.add(&detail_label, 0, SizerFlag::All, 4);
    let detail = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    crate::msaa_names::set_name(&detail, &crate::tr("Error details"));
    sizer.add(&detail, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let refresh_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Refresh Selected"))
        .build();
    let copy_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Copy Details"))
        .build();
    let props_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Feed Properties..."))
        .build();
    let remove_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Remove Feed"))
        .build();
    let close_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Close"))
        .build();
    for b in [&refresh_btn, &copy_btn, &props_btn, &remove_btn, &close_btn] {
        btn_row.add(b, 0, SizerFlag::All, 4);
    }
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(840, 580));

    let errors: Rc<RefCell<Vec<blindrss_core::db::FeedError>>> = Rc::new(RefCell::new(Vec::new()));
    let busy = Rc::new(RefCell::new(false));
    let selected_feed_id: Rc<dyn Fn() -> Option<String>> = {
        let errors = errors.clone();
        Rc::new(move || {
            let sel = list.get_first_selected_item();
            if sel < 0 {
                return None;
            }
            errors.borrow().get(sel as usize).map(|e| e.feed_id.clone())
        })
    };
    // Reload the report from the database and restore the
    // selection (the feed that was selected, else the row now
    // in its slot, clamped — Python's behavior after a retry).
    let reload: Rc<dyn Fn(Option<usize>)> = {
        let errors = errors.clone();
        Rc::new(move |want: Option<usize>| {
            let fresh = Db::open(&crate::db_path())
                .ok()
                .and_then(|db| db.feed_errors().ok())
                .unwrap_or_default();
            heading.set_label(&feed_errors_heading(fresh.len()));
            list.delete_all_items();
            for (i, e) in fresh.iter().enumerate() {
                let i = i as i64;
                list.insert_item(i, &e.title, None);
                list.set_item_text_by_column(i, 1, &feed_error_ts(e.error_at));
                list.set_item_text_by_column(i, 2, &e.consecutive_failures.to_string());
                list.set_item_text_by_column(i, 3, &feed_error_oneline(&e.error));
            }
            let has = !fresh.is_empty();
            *errors.borrow_mut() = fresh;
            refresh_btn.enable(has);
            copy_btn.enable(has);
            props_btn.enable(has);
            remove_btn.enable(has);
            if !has {
                detail.set_value(&crate::tr(
                    "No feeds reported errors during their most recent update.\n\nWhen a feed cannot be updated, its error appears here with options to refresh it, copy the details, open its properties, or remove it.",
                ));
                return;
            }
            let sel = want
                .unwrap_or(0)
                .min(errors.borrow().len().saturating_sub(1));
            list.set_item_state(
                sel as i64,
                ListItemState::Selected | ListItemState::Focused,
                ListItemState::Selected | ListItemState::Focused,
            );
            if let Some(e) = errors.borrow().get(sel) {
                detail.set_value(&feed_error_detail(e));
            }
        })
    };
    reload(None);
    {
        let errors = errors.clone();
        list.on_item_selected(move |_| {
            let sel = list.get_first_selected_item();
            if sel >= 0
                && let Some(e) = errors.borrow().get(sel as usize)
            {
                detail.set_value(&feed_error_detail(e));
            }
        });
    }
    copy_btn.on_click(move |_| {
        let _ = Clipboard::get().set_text(&detail.get_value());
    });
    // Retry: a real single-feed refresh on a worker, using the
    // main refresh path's knobs (global delete behavior,
    // retention cutoff, forced). The drain semantics mirror
    // the main window's: success clears the stored error,
    // failure records the new one.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let done_rx = Rc::new(RefCell::new(done_rx));
    let timer = Box::leak(Box::new(Timer::new(&dialog)));
    timer.start(200, false);
    {
        let reload = reload.clone();
        let busy = busy.clone();
        let errors_tick = errors.clone();
        timer.on_tick(move |_| {
            if done_rx.borrow().try_recv().is_err() {
                return;
            }
            *busy.borrow_mut() = false;
            refresh_btn.enable(true);
            // Re-select the refreshed feed when it still
            // errors, else keep the slot it occupied.
            let slot = list.get_first_selected_item().max(0) as usize;
            reload(Some(slot));
            list.set_focus();
            if errors_tick.borrow().is_empty() {
                close_btn.set_focus();
            }
        });
    }
    let do_refresh: Rc<dyn Fn()> = {
        let busy = busy.clone();
        let selected_feed_id = selected_feed_id.clone();
        let errors = errors.clone();
        Rc::new(move || {
            if *busy.borrow() {
                return;
            }
            let Some(feed_id) = selected_feed_id() else {
                return;
            };
            let title = errors
                .borrow()
                .iter()
                .find(|e| e.feed_id == feed_id)
                .map(|e| e.title.clone())
                .unwrap_or_default();
            *busy.borrow_mut() = true;
            refresh_btn.enable(false);
            heading.set_label(&format!("{} '{title}'...", crate::tr("Refreshing")));
            let done_tx = done_tx.clone();
            std::thread::spawn(move || {
                if let Ok(db) = Db::open(&crate::db_path())
                    && let Ok(feeds) = db.feeds()
                    && let Some(feed) = feeds.iter().find(|f| f.id == feed_id)
                {
                    let cfg = Config::load(&crate::config_path()).unwrap_or_default();
                    let behavior = cfg.get_str("delete_behavior", "deleted").to_string();
                    let cutoff = crate::article_retention_cutoff();
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    let result = blindrss_core::refresh::refresh_feed_with_options(
                        &db,
                        feed,
                        &behavior,
                        cutoff.as_deref(),
                        true,
                        &cancel,
                    );
                    match &result {
                        Ok(_) => {
                            let _ = db.clear_feed_error(&feed.id);
                        }
                        Err(e) => {
                            let _ = db.record_feed_error(&feed.id, e);
                        }
                    }
                }
                let _ = done_tx.send(());
            });
        })
    };
    {
        let do_refresh = do_refresh.clone();
        refresh_btn.on_click(move |_| do_refresh());
    }
    list.on_item_activated(move |_| do_refresh());
    {
        let frame_p = *frame;
        let changed = changed.clone();
        let selected_feed_id = selected_feed_id.clone();
        props_btn.on_click(move |_| {
            let Some(feed_id) = selected_feed_id() else {
                return;
            };
            if let Ok(db) = Db::open(&crate::db_path())
                && let Ok(feeds) = db.feeds()
                && let Some(feed) = feeds.iter().find(|f| f.id == feed_id)
                && show_feed_properties(&frame_p, &db, feed)
            {
                *changed.borrow_mut() = true;
            }
        });
    }
    {
        let changed = changed.clone();
        let reload = reload.clone();
        let selected_feed_id = selected_feed_id.clone();
        remove_btn.on_click(move |_| {
            let Some(feed_id) = selected_feed_id() else {
                return;
            };
            let slot = list.get_first_selected_item().max(0) as usize;
            if let Ok(db) = Db::open(&crate::db_path()) {
                let _ = db.remove_feed(&feed_id);
                *changed.borrow_mut() = true;
            }
            reload(Some(slot));
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5100));
    dialog.show_modal();
    dialog.destroy();
    *changed.borrow()
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
/// Play (button and Enter) goes through `on_play`, the main
/// window's normal article activation, so downloaded and
/// recovered episodes play in the player exactly like list
/// articles (Python's `_on_play` -> host `_open_article`;
/// Audit 8 Task 27, F04).
pub fn show_podcast_archive(
    frame: &Frame,
    db: &Db,
    on_play: Rc<dyn Fn(&blindrss_core::Article)>,
) -> Option<blindrss_core::Feed> {
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
    dialog.set_escape_id(5101);
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
                let downloaded = crate::downloaded_path_for(&cfg, article).is_some();
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
                // The standard feed-folder path, like Python's
                // archive worker (its `_downloaded_media_path_
                // for_article` chain): the shared download root
                // plus the feed-title folder override (Audit 8
                // Task 27, resolving Task 22's deviation).
                let root = crate::download_root(&cfg);
                let folder_override = if feed_title.is_empty() {
                    "Podcast Archive"
                } else {
                    &feed_title
                };
                let folder = blindrss_core::download::download_dir_for(
                    &root,
                    Some(&feed_title),
                    Some(folder_override),
                );
                let _ = std::fs::create_dir_all(&folder);
                let retention_secs = crate::download_retention_seconds(&cfg);
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
                    if crate::downloaded_path_for(&cfg, article).is_some() {
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
                            blindrss_core::download::apply_download_retention(
                                &folder,
                                retention_secs,
                            );
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
                on_play(&article);
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
    // The ONE busy-close handler (Python's `_on_close`,
    // EVT_CLOSE): every close route — the Close button,
    // Escape, Alt+F4, the title-bar button — funnels through
    // it, so a scan or download in flight always gets the
    // stop-and-close-when-safe confirmation (Audit 8 Task
    // 27, F15). Native closes are vetoed and re-driven
    // through end_modal so the modal result matches the
    // Close button's.
    let request_close: Rc<dyn Fn()> = {
        let scan_running = scan_running.clone();
        let download_running = download_running.clone();
        let close_when_idle = close_when_idle.clone();
        let scan_cancel = scan_cancel.clone();
        let download_cancel = download_cancel.clone();
        let frame2 = *frame;
        let d = dialog;
        Rc::new(move || {
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
        })
    };
    {
        let request_close = request_close.clone();
        close_btn.on_click(move |_| request_close());
    }
    dialog.bind_internal(EventType::CLOSE_WINDOW, move |ev| {
        // Only a vetoable close can be re-driven through the
        // stop-and-close confirmation; a forced close (app
        // shutdown, session end) must proceed natively —
        // Veto() on a non-vetoable event breaks wx's contract
        // (the 2026-11-07 debugging-check dialog).
        if ev.can_veto() {
            ev.veto();
            request_close();
        }
    });

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
            blindrss_core::download::download_with_ytdlp(&article.url, dir, format, &article.title)
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
/// returns the chosen entry index and closes. `states` is the
/// caller's single-scan playback-state map (Audit 8 G11):
/// queue rows carry Python's shared time annotation, composed
/// here from the precomputed map — no per-row lookups.
pub fn show_queue(
    frame: &Frame,
    config_path: &Path,
    states: &std::collections::HashMap<String, blindrss_core::PlaybackState>,
) -> Option<usize> {
    let entries = Rc::new(RefCell::new(load_queue(config_path)));
    let dialog = Dialog::builder(frame, "Play Queue").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "play-queue");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let states = Rc::new(states.clone());
    let reload = {
        let entries = entries.clone();
        let states = states.clone();
        move |select: Option<usize>| {
            list.clear();
            for e in entries.borrow().iter() {
                let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let feed = e.get("feed_title").and_then(|v| v.as_str()).unwrap_or("");
                let base = if feed.is_empty() {
                    title.to_string()
                } else {
                    format!("{title} - {feed}")
                };
                let state = e.get("article_id").and_then(|v| v.as_str()).and_then(|id| {
                    blindrss_core::playback::state_for_article(
                        &states,
                        id,
                        e.get("media_url").and_then(|v| v.as_str()),
                        e.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                    )
                });
                list.append(&format!(
                    "{base}, {}",
                    blindrss_core::playback::playback_time_annotation(state)
                ));
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

/// One Takeout source group's dialog label (Python
/// `mainframe._show_youtube_takeout_options`).
fn takeout_group_label(source: &str, count: usize) -> String {
    match source {
        "subscriptions" => format!("Subscribed channels ({count})"),
        "history" => format!("Channels from watch history ({count})"),
        "owner" => format!("Owned channels ({count})"),
        "playlists" => format!("Playlists ({count})"),
        _ => format!("{source} ({count})"),
    }
}

/// Takeout import selection (Python's
/// TakeoutImportSelectionDialog): the source groups found in
/// the Takeout ZIP, each with its count, all checked at
/// first; the checked groups import. Python moved this
/// dialog off wx.CheckListBox — an owner-drawn list whose
/// checkboxes expose no checkable state to MSAA/UIA
/// (nvaccess/nvda#7325) — onto a native report list with
/// real checkbox state. wxDragon's ListCtrl has no checkbox
/// binding, so this uses the one wxDragon control with
/// native checkbox state, a TreeListCtrl in Checkbox style
/// with flat root items (Audit 8 Task 27, F12; the control
/// role differs from Python's list — NVDA checked-state and
/// Space acceptance is the owner's manual item, recorded
/// in PARITY.md). Returns the chosen source keys, or None
/// on cancel.
pub fn select_takeout_sources(
    frame: &Frame,
    import: &blindrss_core::takeout::TakeoutImport,
) -> Option<Vec<String>> {
    use wxdragon::widgets::treelistctrl::{
        CheckboxState, TreeListCtrl, TreeListCtrlStyle, TreeListItem,
    };
    let dialog = Dialog::builder(frame, "Import YouTube Takeout").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "TakeoutImportSelectionDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Select what to import from this YouTube Takeout file. Press Space to check or uncheck an item:",
        ))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = TreeListCtrl::builder(&dialog)
        .with_style(TreeListCtrlStyle::Default | TreeListCtrlStyle::Checkbox)
        .build();
    list.set_name("YouTube Takeout items to import");
    list.append_column("", 480, ListColumnFormat::Left);
    let root = list.get_root_item();
    let mut items: Vec<(String, TreeListItem)> = Vec::new();
    for source in import.groups() {
        let text = takeout_group_label(source, import.count(source));
        if let Some(item) = list.append_item(&root, &text) {
            list.check_item(&item, CheckboxState::Checked);
            if items.is_empty() {
                list.select_item(&item);
            }
            items.push((source.to_string(), item));
        }
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 300));
    list.set_focus();
    let result = if dialog.show_modal() == 5100 {
        Some(
            items
                .iter()
                .filter(|(_, item)| list.is_checked(item))
                .map(|(source, _)| source.clone())
                .collect(),
        )
    } else {
        None
    };
    dialog.destroy();
    result
}

/// Takeout destination category (Python's category step in
/// `_show_youtube_takeout_options`): a single choice over
/// the active provider's categories, with the Uncategorized
/// sentinel first when the provider does not list it,
/// duplicates removed. Returns the chosen category name, or
/// None on cancel.
pub fn choose_takeout_category(frame: &Frame, categories: &[String]) -> Option<String> {
    let mut choices: Vec<String> = Vec::new();
    if !categories
        .iter()
        .any(|c| c == blindrss_core::opml::UNCATEGORIZED)
    {
        choices.push(blindrss_core::opml::UNCATEGORIZED.to_string());
    }
    for c in categories {
        if !choices.contains(c) {
            choices.push(c.clone());
        }
    }
    let dialog = Dialog::builder(frame, "Import YouTube Takeout").build();
    dialog.set_escape_id(5101);
    bind_f1(frame, &dialog, "TakeoutCategoryDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Choose the destination category:"))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    list.set_name("Destination category");
    for c in &choices {
        list.append(c);
    }
    if !choices.is_empty() {
        list.set_selection(0, true);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 300));
    list.set_focus();
    let result = if dialog.show_modal() == 5100 {
        list.get_selection()
            .and_then(|i| choices.get(i as usize).cloned())
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
/// counted separately and named in the result (no elevation
/// machinery in the Rust app — Audit 8 B17); the appended file
/// import below is their working path. Firefox-family profiles
/// import directly (B03), and the browser User-Agent can be
/// edited on its own (B08). Accessibility: the Python-shaped
/// controls are APPENDED after the shipped Chromium controls, so
/// the existing tab order is unchanged; the new text fields carry
/// accessible names only.
pub fn show_import_cookies(frame: &Frame, config_path: &Path) {
    use blindrss_core::chromium;
    let dialog = Dialog::builder(frame, "Import Site Cookies").build();
    dialog.set_escape_id(5101);
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
    // Python's dialog also carries an exported-file import and
    // the browser User-Agent field (Audit 8 B08), appended after
    // the shipped controls so their tab order is unchanged. The
    // file import is the working path for cookies no direct
    // reader can decrypt (Chrome v20 app-bound, B17), and for
    // browsers with no direct reader at all.
    let file_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Cookies file:"))
        .build();
    sizer.add(&file_label, 0, SizerFlag::All, 4);
    let file_row = BoxSizer::builder(Orientation::Horizontal).build();
    let file_ctrl = TextCtrl::builder(&dialog).build();
    file_ctrl.set_name("Cookies file path");
    let browse_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Browse..."))
        .build();
    file_row.add(&file_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    file_row.add(&browse_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&file_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let ua_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Browser User-Agent (recommended):"))
        .build();
    sizer.add(&ua_label, 0, SizerFlag::All, 4);
    let ua_ctrl = TextCtrl::builder(&dialog).build();
    ua_ctrl.set_name("Browser User-Agent");
    let stored_ua = blindrss_core::cookies::get_global_user_agent(config_path);
    let initial_ua = if stored_ua.is_empty() {
        blindrss_core::http::user_agent()
    } else {
        stored_ua
    };
    ua_ctrl.set_value(&initial_ua);
    sizer.add(&ua_ctrl, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let file_btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let firefox_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Import from Firefox..."))
        .build();
    let file_import_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Import File / Save User-Agent"))
        .build();
    file_btn_row.add(&firefox_btn, 0, SizerFlag::All, 4);
    file_btn_row.add(&file_import_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&file_btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(620, 680));

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let tx_file = tx.clone();
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
            let cfg_thread = cfg_path.clone();
            let jar_path = cfg_path
                .parent()
                .map(|d| d.join("site_cookies.txt"))
                .unwrap_or_default();
            std::thread::spawn(move || {
                let mut records = Vec::new();
                let mut skipped = 0usize;
                let mut skipped_v20 = 0usize;
                // B17: one batched elevation resolves the
                // chosen profiles' v20 keys and stages any
                // locked databases (Python's import flow).
                let data_dir = cfg_thread.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                let chosen_refs: Vec<&chromium::ChromiumProfile> = chosen.iter().collect();
                let prep = blindrss_core::chromium_v20::prepare_v20_reads(&chosen_refs, &data_dir);
                for (index, p) in chosen.iter().enumerate() {
                    let outcome = chromium::read_profile_cookies_resolved(
                        p,
                        prep.key_for(p),
                        prep.staged.get(&index).map(|s| s.as_path()),
                    );
                    skipped += outcome.skipped;
                    skipped_v20 += outcome.skipped_v20;
                    // Pin this profile's clearance hosts to the
                    // UA its cookies were issued to (B05).
                    let ua = chromium::chromium_profile_user_agent(p);
                    if !ua.is_empty() {
                        for record in &outcome.records {
                            if blindrss_core::cookies::is_clearance_cookie_name(&record.name) {
                                let host = record.host.trim().trim_start_matches('.');
                                if !host.is_empty() {
                                    blindrss_core::cookies::set_host_user_agent(
                                        &cfg_thread,
                                        host,
                                        &ua,
                                    );
                                }
                            }
                        }
                    }
                    records.extend(outcome.records);
                }
                prep.cleanup();
                let existing = std::fs::read_to_string(&jar_path).unwrap_or_default();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let merged = chromium::merge_into_jar(&existing, &records, now);
                let imported = records.len();
                let mut msg = match std::fs::write(&jar_path, merged) {
                    Ok(()) => {
                        let mut text = format!(
                            "Imported {imported} cookies from {} profiles; {skipped} skipped (could not be decrypted).",
                            chosen.len()
                        );
                        // This is an explicit full import, so
                        // YouTube sessions also land in yt-dlp's
                        // cookie file (B04).
                        let mut cfg = Config::load(&cfg_thread).unwrap_or_default();
                        if let Some(data_dir) = cfg_thread.parent() {
                            let yt = blindrss_core::cookies_import::merge_youtube_cookies(
                                &mut cfg,
                                data_dir,
                                &records,
                                now,
                            );
                            if yt > 0 && cfg.save(&cfg_thread).is_ok() {
                                text +=
                                    &format!(" {yt} YouTube cookies also saved for yt-dlp.");
                            }
                        }
                        text
                    }
                    Err(e) => format!("Could not write the cookie jar: {e}"),
                };
                if skipped_v20 > 0 {
                    msg += &format!(
                        " {skipped_v20} cookies use Chrome's app-bound (v20) encryption and could not be read this time (their key elevation was cancelled or failed); try the import again and accept the Windows prompt, or export a cookies.txt from that browser and use the file import below."
                    );
                }
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
    {
        let frame_browse = *frame;
        browse_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_browse)
                .with_message("Select a cookies.txt export")
                .with_wildcard("Cookie files (*.txt)|*.txt")
                .with_style(FileDialogStyle::Open)
                .build();
            if dlg.show_modal() == 5100
                && let Some(path) = dlg.get_path()
            {
                file_ctrl.set_value(&path);
            }
        });
    }
    {
        let cfg_path = config_path.to_path_buf();
        let tx = tx_file.clone();
        file_import_btn.on_click(move |_| {
            let src = file_ctrl.get_value();
            let ua = ua_ctrl.get_value();
            status.set_label("Importing...");
            let tx = tx.clone();
            let cfg_path = cfg_path.clone();
            let jar_path = cfg_path
                .parent()
                .map(|d| d.join("site_cookies.txt"))
                .unwrap_or_default();
            std::thread::spawn(move || {
                // Python's OK semantics: an empty file field
                // saves the User-Agent alone; an invalid file is
                // an error and NOTHING is saved.
                let msg = if src.trim().is_empty() {
                    blindrss_core::cookies::set_global_user_agent(&cfg_path, &ua);
                    "Saved the browser User-Agent.".to_string()
                } else {
                    match blindrss_core::cookies_import::merge_jar_file(
                        &jar_path,
                        std::path::Path::new(&src),
                    ) {
                        Ok(n) => {
                            blindrss_core::cookies::set_global_user_agent(&cfg_path, &ua);
                            format!(
                                "Imported {n} cookies from the file and saved the browser User-Agent."
                            )
                        }
                        Err(e) => {
                            format!("Import failed: {e} Nothing was changed.")
                        }
                    }
                };
                let _ = tx.send(msg);
            });
        });
    }
    {
        let cfg_path = config_path.to_path_buf();
        let frame_ff = *frame;
        firefox_btn.on_click(move |_| {
            let profiles = blindrss_core::firefox::list_profiles();
            if profiles.is_empty() {
                let md = MessageDialog::builder(
                    &frame_ff,
                    "No Firefox-family browser profiles were found. Firefox, LibreWolf, Waterfox, Floorp and Zen are read directly; for Chrome, Edge or Brave, export a cookies.txt from the browser and use the file import instead.",
                    "Import Site Cookies",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                md.destroy();
                return;
            }
            let pick = if profiles.len() == 1 {
                Some(0)
            } else {
                pick_firefox_profile(&frame_ff, &profiles)
            };
            let Some(idx) = pick else { return };
            let profile = &profiles[idx];
            let jar_path = cfg_path
                .parent()
                .map(|d| d.join("site_cookies.txt"))
                .unwrap_or_default();
            match blindrss_core::firefox::read_profile_cookies(profile) {
                Ok(records) => {
                    let existing = std::fs::read_to_string(&jar_path).unwrap_or_default();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let count = records.len();
                    let merged = chromium::merge_into_jar(&existing, &records, now);
                    match std::fs::write(&jar_path, merged) {
                        Ok(()) => {
                            // Python's dialog flow: the profile's
                            // UA becomes the stored browser UA.
                            let ua =
                                blindrss_core::firefox::profile_user_agent(&profile.profile_dir);
                            if !ua.is_empty() {
                                blindrss_core::cookies::set_global_user_agent(&cfg_path, &ua);
                                ua_ctrl.set_value(&ua);
                            }
                            // An explicit full import, so YouTube
                            // records also land in yt-dlp's jar
                            // (B04, as with the Chromium import).
                            let mut extra = String::new();
                            let mut cfg = Config::load(&cfg_path).unwrap_or_default();
                            if let Some(data_dir) = cfg_path.parent() {
                                let yt = blindrss_core::cookies_import::merge_youtube_cookies(
                                    &mut cfg,
                                    data_dir,
                                    &records,
                                    now,
                                );
                                if yt > 0 && cfg.save(&cfg_path).is_ok() {
                                    extra =
                                        format!(" {yt} YouTube cookies also saved for yt-dlp.");
                                }
                            }
                            status.set_label(&format!(
                                "Imported {count} cookies from {} — {}.{extra}",
                                profile.browser, profile.profile
                            ));
                        }
                        Err(e) => {
                            status.set_label(&format!("Could not write the cookie jar: {e}"))
                        }
                    }
                }
                Err(e) => status.set_label(&format!("Could not read that profile: {e}")),
            }
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

/// Pick one Firefox-family profile when several exist (Python's
/// SingleChoiceDialog step in the Import Site Cookies flow).
fn pick_firefox_profile(
    frame: &Frame,
    profiles: &[blindrss_core::firefox::FirefoxProfile],
) -> Option<usize> {
    let dialog = Dialog::builder(frame, "Import from Firefox").build();
    dialog.set_escape_id(5101);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Choose the Firefox profile to import from:"))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    list.set_name("Firefox profile");
    for p in profiles {
        list.append(&format!("{} — {}", p.browser, p.profile));
    }
    list.set_selection(0, true);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 360));
    let result = if dialog.show_modal() == 5100 {
        list.get_selection().map(|s| s as usize)
    } else {
        None
    };
    dialog.destroy();
    result
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
    dialog.set_escape_id(5101);
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
    dialog.set_escape_id(5101);
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
    use super::{
        category_edited_path, category_leaf_parent, category_parent_choices,
        feed_error_failures_line, feed_error_oneline, feed_errors_heading,
        normalize_saved_searches, saved_search_present,
    };

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

    #[test]
    fn saved_searches_normalize_like_python() {
        // Audit 8 G17: trim, drop empties, dedupe
        // case-insensitively keeping the first.
        let raw: Vec<String> = [" News ", "news", "", "  ", "Tech", "TECH"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            normalize_saved_searches(&raw),
            vec!["News".to_string(), "Tech".to_string()]
        );
        assert!(saved_search_present(&raw, " NEWS "));
        assert!(saved_search_present(&raw, ""));
        assert!(!saved_search_present(&raw, "Sports"));
    }

    #[test]
    fn feed_error_texts_match_python_shape() {
        // Audit 8 G10 pure builders (defined with
        // show_feed_errors below).
        assert_eq!(
            feed_errors_heading(0),
            "No feeds reported errors during their most recent update."
        );
        assert_eq!(
            feed_errors_heading(2),
            "2 feed(s) failed to update during their most recent attempt:"
        );
        assert_eq!(feed_error_oneline("a\n\n  b\tc"), "a b c");
        let long = "x".repeat(300);
        let cut = feed_error_oneline(&long);
        assert_eq!(cut.chars().count(), 221);
        assert!(cut.ends_with('…'));
        assert_eq!(
            feed_error_failures_line(1),
            "1 failed attempt so far (this may be a one-off glitch)."
        );
        assert_eq!(
            feed_error_failures_line(3),
            "3 consecutive failed attempt(s) (likely a persistent problem)."
        );
    }
}
