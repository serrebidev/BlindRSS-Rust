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
            .with_label("Show articles that match")
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
        let add_btn = Button::builder(dialog).with_label("Add Condition").build();
        let remove_btn = Button::builder(dialog)
            .with_label("Remove Condition")
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
    let ok_btn = Button::builder(dialog).with_label("OK").build();
    let cancel_btn = Button::builder(dialog).with_label("Cancel").build();
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
        .with_label("When an article matches:")
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
pub fn show_filter_rules(frame: &Frame, db: &Db) {
    let dialog = Dialog::builder(frame, "Filter Rules").build();
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
            Ok(db) => match blindrss_core::refresh::apply_rules_to_existing(&db) {
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
pub fn show_feed_properties(frame: &Frame, db: &Db, feed: &blindrss_core::Feed) -> bool {
    let dialog = Dialog::builder(frame, "Feed Properties").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let title_ctrl = name_row(&dialog, &sizer, "Title:", &feed.title);
    let url_ctrl = name_row(&dialog, &sizer, "Feed URL:", &feed.url);
    let cat0 = feed.category.clone().unwrap_or_default();
    let cat_ctrl = name_row(&dialog, &sizer, "Category (blank = none):", &cat0);
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
        .with_label("Reset Title to Feed's Own")
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
    let ok_btn = Button::builder(&dialog).with_label("OK").build();
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
    let frame = Frame::builder()
        .with_title(&format!("BlindRSS - {title}"))
        .with_size(Size::new(760, 560))
        .build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let reader = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    reader.set_value(body);
    sizer.add(&reader, 1, SizerFlag::Expand | SizerFlag::All, 4);
    frame.set_sizer(sizer, true);
    frame.show(true);
    reader.set_focus();
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

/// Podcast archive manager (Python's PodcastArchiveDialog):
/// lists feeds that carry media with their episode counts and
/// lets the user pick one to rescan against the Wayback
/// Machine. Returns the chosen feed; the caller runs the
/// rescan on a worker (it does network I/O).
pub fn show_podcast_archive(frame: &Frame, db: &Db) -> Option<blindrss_core::Feed> {
    let feeds = db.feeds().ok()?;
    let all = db.articles(None, 100_000).ok()?;
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for a in &all {
        if a.media_url.is_some() {
            *counts.entry(a.feed_id.as_str()).or_default() += 1;
        }
    }
    let podcasts: Vec<&blindrss_core::Feed> = feeds
        .iter()
        .filter(|f| counts.contains_key(f.id.as_str()))
        .collect();
    if podcasts.is_empty() {
        let md = MessageDialog::builder(
            frame,
            "No feeds with media episodes were found.",
            "Podcast Archive",
        )
        .with_style(MessageDialogStyle::OK)
        .build();
        md.show_modal();
        md.destroy();
        return None;
    }
    let dialog = Dialog::builder(frame, "Podcast Archive").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListBox::builder(&dialog).build();
    for f in &podcasts {
        list.append(&format!(
            "{} ({} episodes)",
            f.title,
            counts.get(f.id.as_str()).copied().unwrap_or(0)
        ));
    }
    list.set_selection(0, true);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let rescan_btn = Button::builder(&dialog)
        .with_label("Rescan Archive")
        .build();
    let close_btn = Button::builder(&dialog).with_label("Close").build();
    row.add(&rescan_btn, 0, SizerFlag::All, 4);
    row.add(&close_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let chosen: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    {
        let chosen = chosen.clone();
        let d = dialog;
        rescan_btn.on_click(move |_| {
            *chosen.borrow_mut() = list.get_selection().map(|s| s as usize);
            d.end_modal(5100);
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 420));
    let idx = if dialog.show_modal() == 5100 {
        *chosen.borrow()
    } else {
        None
    };
    dialog.destroy();
    idx.and_then(|i| podcasts.get(i).map(|f| (*f).clone()))
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
        .with_label("Choose the subscriptions to import:")
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
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let url_ctrl = name_row(&dialog, &sizer, "Site or feed URL:", "");
    let list = ListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let status = StaticText::builder(&dialog).with_label("").build();
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

    let candidates: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
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
            status2.set_label("Searching...");
            let tx = tx.clone();
            std::thread::spawn(move || {
                let result = blindrss_core::discovery::resolve_subscribe_url(&url)
                    .map(|(feed_url, _)| vec![feed_url])
                    .or_else(|_| {
                        blindrss_core::extract::fetch_html(&url)
                            .map(|html| blindrss_core::discovery::discover_in_html(&html, &url))
                    });
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
                *chosen.borrow_mut() = candidates.borrow().get(sel as usize).cloned();
            }
            if chosen.borrow().is_some() {
                d.end_modal(5100);
            }
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));

    // Drain worker results while the dialog is open.
    let timer = Box::leak(Box::new(Timer::new(&dialog)));
    {
        let rx = rx.clone();
        let candidates = candidates.clone();
        let list2 = list;
        let status3 = status;
        timer.on_tick(move |_| {
            while let Ok(result) = rx.try_recv() {
                match result {
                    Ok(urls) => {
                        list2.clear();
                        for u in &urls {
                            list2.append(u);
                        }
                        status3.set_label(&format!("{} feed(s) found", urls.len()));
                        *candidates.borrow_mut() = urls;
                    }
                    Err(e) => status3.set_label(&format!("Search failed: {e}")),
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
        .with_label("Checked feeds will not send notifications:")
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
