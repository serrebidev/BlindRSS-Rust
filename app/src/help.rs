//! Help viewer dialog: the complete guide in ONE read-only
//! text control (continuous reading across topics), a contents
//! list that jumps by character offset, and Find with
//! wrapping (Audit 8 S07/S08; Python gui/help_viewer.py over
//! core/help_docs.py). The guide shown is the locale fallback
//! chain regional → base → English; when the English text is
//! shown for another requested language, a notice at the top
//! says so, as Python's viewer does.

use patois::t;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;

pub fn help_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BLINDRSS_HELP") {
        return Some(PathBuf::from(p));
    }
    let mut candidates = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/help")];
    // Release layouts: docs/help beside the executable, or in
    // the .app bundle's Resources (Audit 8 B14).
    candidates.extend(
        crate::resource_dirs()
            .into_iter()
            .map(|d| d.join("docs/help")),
    );
    candidates.into_iter().find(|p| p.join("en.md").exists())
}

pub fn show_help(frame: &Frame, locale: &str) {
    show_help_topic(frame, locale, None);
}

/// The changelog embedded in the binary (app/build.rs points
/// cargo at the repo-root CHANGELOG.md), so Help > View
/// Changelog works fully offline — the BlindRadio pattern,
/// adapted to BlindRSS's native text viewer.
const CHANGELOG_MD: &str = include_str!("../../CHANGELOG.md");

/// Help > View Changelog (Brandon's request, 2026-11-10):
/// the FULL release history — every version, including the
/// ones an update skipped over — in the same viewer shape
/// as the guide: a versions list that jumps the reader,
/// Find with wrapping, Escape to close.
pub fn show_changelog(frame: &Frame) {
    let sections = blindrss_core::changelog::parse_changelog(CHANGELOG_MD);
    let text = blindrss_core::changelog::render_changelog_text(&sections);
    let versions: Vec<String> = sections
        .iter()
        .map(|s| format!("BlindRSS version {}", s.version))
        .collect();
    // Character offset where each version's heading starts
    // in the rendered text; the versions list jumps by these.
    let offsets: Vec<usize> = {
        let mut starts = Vec::with_capacity(versions.len());
        let mut from = 0usize;
        for heading in &versions {
            match text[from..].find(heading.as_str()) {
                Some(at) => {
                    starts.push(from + at);
                    from += at + heading.len();
                }
                None => starts.push(from),
            }
        }
        starts
    };
    show_text_viewer(
        frame,
        &t("BlindRSS Changelog"),
        &text,
        &versions,
        &offsets,
        &t("Changelog versions"),
        &t("Changelog text"),
        &t("Wrapped to the start of the changelog."),
        &t("Wrapped to the end of the changelog."),
    );
}

/// The shared read-only text viewer behind the guide and
/// the changelog: Find row, a jump list beside the reader,
/// Close. `list_labels` and `offsets` are parallel: choosing
/// a list entry moves the reader caret to that character
/// offset. Extracted from the guide's builder unchanged in
/// behaviour (Audit 8 S07/S08 shape).
#[allow(clippy::too_many_arguments)]
fn show_text_viewer(
    frame: &Frame,
    title: &str,
    text: &str,
    list_labels: &[String],
    offsets: &[usize],
    list_name: &str,
    reader_name: &str,
    wrap_start_msg: &str,
    wrap_end_msg: &str,
) {
    let text: Rc<String> = Rc::new(text.to_string());
    let dialog = Dialog::builder(frame, title).build();
    dialog.set_escape_id(5101);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    // Find row (Python's order: find controls above the body).
    let find_row = BoxSizer::builder(Orientation::Horizontal).build();
    let find_label = StaticText::builder(&dialog).with_label(&t("Find:")).build();
    let find_ctrl = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    crate::msaa_names::set_name(&find_ctrl, &t("Find in text"));
    let find_next_btn = Button::builder(&dialog).with_label(&t("Find Next")).build();
    let find_prev_btn = Button::builder(&dialog)
        .with_label(&t("Find Previous"))
        .build();
    find_row.add(&find_label, 0, SizerFlag::All, 4);
    find_row.add(&find_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    find_row.add(&find_next_btn, 0, SizerFlag::All, 4);
    find_row.add(&find_prev_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&find_row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let list = ListBox::builder(&dialog).build();
    crate::msaa_names::set_name(&list, list_name);
    for label in list_labels {
        list.append(label);
    }
    let reader = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::Rich2)
        .build();
    crate::msaa_names::set_name(&reader, reader_name);
    if text.is_empty() {
        reader.set_value(&t("The text could not be loaded."));
    } else {
        reader.set_value(&text);
    }
    row.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 1, SizerFlag::Expand | SizerFlag::All, 2);
    let close_btn = Button::builder(&dialog).with_label(&t("Close")).build();
    sizer.add(&close_btn, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(760, 520));

    // The reader opens at the top; the first list entry is
    // the matching position.
    reader.set_insertion_point(0);
    if !list_labels.is_empty() {
        list.set_selection(0, true);
    }

    // List selection jumps the reader to the entry's offset.
    // No announcement: the reader's new position speaks for
    // itself (Python parity).
    {
        let offsets: Vec<i64> = offsets.iter().map(|&o| o as i64).collect();
        list.on_selection_changed(move |e| {
            if let Some(sel) = e.get_selection()
                && let Some(&offset) = offsets.get(sel as usize)
            {
                reader.set_insertion_point(offset);
                reader.set_focus();
            }
        });
    }

    // Find over the whole text (core textfind holds the
    // policy; Python gui/help_viewer.py:220): forward from
    // just after the caret, backward to the last match ending
    // at or before it, wrapping once; the match is selected,
    // focus returns to the text, and wraps / misses are
    // announced.
    let search_text = Rc::new(text.to_lowercase());
    let wrap_start = wrap_start_msg.to_string();
    let wrap_end = wrap_end_msg.to_string();
    let do_find: Rc<dyn Fn(bool)> = {
        let search_text = search_text.clone();
        Rc::new(move |forward: bool| {
            let term = find_ctrl.get_value();
            let term = term.trim().to_lowercase();
            if term.is_empty() {
                find_ctrl.set_focus();
                return;
            }
            let caret = reader.get_insertion_point().max(0) as usize;
            match blindrss_core::textfind::find_in_guide(&search_text, &term, caret, forward) {
                Some((start, wrapped)) => {
                    let end = start + term.chars().count();
                    reader.set_selection(start as i64, end as i64);
                    reader.set_insertion_point(end as i64);
                    reader.set_focus();
                    if wrapped {
                        crate::announce("general", if forward { &wrap_start } else { &wrap_end });
                    }
                }
                None => {
                    crate::announce("general", &format!("Not found: {term}"));
                }
            }
        })
    };
    {
        let find = do_find.clone();
        find_next_btn.on_click(move |_| find(true));
    }
    {
        let find = do_find.clone();
        find_prev_btn.on_click(move |_| find(false));
    }
    {
        let find = do_find.clone();
        find_ctrl.on_text_enter(move |_| find(true));
    }
    // Ctrl+F focuses the find box with its text selected; F3 /
    // Shift+F3 repeat the search; Escape closes (Python binds
    // the same keys on its help window).
    dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
        let code = ev.get_key_code().unwrap_or(0);
        if code == 27 {
            dialog.end_modal(5100);
            ev.skip(false);
            return;
        }
        let mods = blindrss_core::shortcuts::KeyModifiers {
            ctrl: ev.control_down(),
            alt: ev.alt_down(),
            shift: ev.shift_down(),
            cmd: ev.meta_down(),
        };
        match blindrss_core::shortcuts::event_accel(code, mods, true).as_deref() {
            Some("Ctrl+F") => {
                find_ctrl.set_focus();
                find_ctrl.select_all();
                ev.skip(false);
            }
            Some("F3") => {
                do_find(true);
                ev.skip(false);
            }
            Some("Shift+F3") => {
                do_find(false);
                ev.skip(false);
            }
            _ => ev.skip(true),
        }
    });
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}

/// Help opened at a specific guide section (F1 context help):
/// the topic anchor's line when known, else the default
/// topic's line, else the top.
pub fn show_help_topic(frame: &Frame, locale: &str, topic_id: Option<&str>) {
    let Some(dir) = help_dir() else {
        let md = MessageDialog::builder(frame, "Help files were not found.", "Help")
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
        md.show_modal();
        return;
    };
    let doc = blindrss_core::help::load_document(&dir, locale);
    let rendered = blindrss_core::help::render_plain_text(&doc);
    let contents = blindrss_core::help::contents_entries(&doc);
    // Character offset of each rendered line's start.
    let line_starts: Rc<Vec<usize>> = {
        let mut starts = vec![0usize];
        for (i, ch) in rendered.text.chars().enumerate() {
            if ch == '\n' {
                starts.push(i + 1);
            }
        }
        Rc::new(starts)
    };
    let offset_of_line = {
        let line_starts = line_starts.clone();
        move |line: usize| -> i64 { line_starts.get(line).copied().unwrap_or(0) as i64 }
    };

    let dialog = Dialog::builder(frame, "BlindRSS Help").build();
    dialog.set_escape_id(5101);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    if doc.is_fallback() {
        let notice = StaticText::builder(&dialog)
            .with_label(&t(
                "This guide has not been translated into your language yet, so it is shown in English.",
            ))
            .build();
        crate::msaa_names::set_name(&notice, &t("Guide language"));
        sizer.add(&notice, 0, SizerFlag::All, 4);
    }
    // Find row (Python's order: find controls above the body).
    // NOTE: show_text_viewer below carries this same layout for
    // the changelog; the guide keeps its own builder because its
    // position starts mid-document at a topic line, which the
    // shared shape does not express.
    let find_row = BoxSizer::builder(Orientation::Horizontal).build();
    let find_label = StaticText::builder(&dialog).with_label(&t("Find:")).build();
    let find_ctrl = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    crate::msaa_names::set_name(&find_ctrl, &t("Find in guide"));
    let find_next_btn = Button::builder(&dialog).with_label(&t("Find Next")).build();
    let find_prev_btn = Button::builder(&dialog)
        .with_label(&t("Find Previous"))
        .build();
    find_row.add(&find_label, 0, SizerFlag::All, 4);
    find_row.add(&find_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    find_row.add(&find_next_btn, 0, SizerFlag::All, 4);
    find_row.add(&find_prev_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&find_row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let list = ListBox::builder(&dialog).build();
    crate::msaa_names::set_name(&list, &t("Guide contents"));
    for (_, title, level) in &contents {
        let indent = "    ".repeat(level.saturating_sub(2));
        list.append(&format!("{}{}", indent, t(title)));
    }
    let reader = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::Rich2)
        .build();
    crate::msaa_names::set_name(&reader, &t("User guide text"));
    if rendered.text.is_empty() {
        reader.set_value(&t("The user guide could not be loaded."));
    } else {
        reader.set_value(&rendered.text);
    }
    row.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 1, SizerFlag::Expand | SizerFlag::All, 2);
    let close_btn = Button::builder(&dialog).with_label(&t("Close")).build();
    sizer.add(&close_btn, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(760, 520));

    // Initial position: the requested topic's line, else the
    // default topic's, else the top; the matching contents
    // entry is selected.
    let start_line = topic_id
        .map(blindrss_core::help_topics::normalize)
        .and_then(|anchor| rendered.anchor_lines.get(anchor).copied())
        .or_else(|| {
            rendered
                .anchor_lines
                .get(blindrss_core::help_topics::DEFAULT_TOPIC)
                .copied()
        })
        .unwrap_or(0);
    reader.set_insertion_point(offset_of_line(start_line));
    if let Some(idx) = rendered
        .section_lines
        .iter()
        .rposition(|&line| line <= start_line)
    {
        list.set_selection(idx as u32, true);
    }

    // Contents selection jumps the reader to the section's
    // first line. No announcement: the reader's new position
    // speaks for itself (Python parity).
    {
        let section_lines = rendered.section_lines.clone();
        list.on_selection_changed(move |e| {
            if let Some(sel) = e.get_selection()
                && let Some(&line) = section_lines.get(sel as usize)
            {
                reader.set_insertion_point(offset_of_line(line));
                reader.set_focus();
            }
        });
    }

    // Find over the whole guide (core textfind holds the
    // policy; Python gui/help_viewer.py:220): forward from
    // just after the caret, backward to the last match ending
    // at or before it, wrapping once; the match is selected,
    // focus returns to the guide text, and wraps / misses are
    // announced in Python's words.
    let search_text = Rc::new(rendered.text.to_lowercase());
    let do_find: Rc<dyn Fn(bool)> = {
        let search_text = search_text.clone();
        Rc::new(move |forward: bool| {
            let term = find_ctrl.get_value();
            let term = term.trim().to_lowercase();
            if term.is_empty() {
                find_ctrl.set_focus();
                return;
            }
            let caret = reader.get_insertion_point().max(0) as usize;
            match blindrss_core::textfind::find_in_guide(&search_text, &term, caret, forward) {
                Some((start, wrapped)) => {
                    let end = start + term.chars().count();
                    reader.set_selection(start as i64, end as i64);
                    reader.set_insertion_point(end as i64);
                    reader.set_focus();
                    if wrapped {
                        crate::announce(
                            "general",
                            if forward {
                                "Wrapped to the start of the guide."
                            } else {
                                "Wrapped to the end of the guide."
                            },
                        );
                    }
                }
                None => {
                    crate::announce("general", &format!("Not found: {term}"));
                }
            }
        })
    };
    {
        let find = do_find.clone();
        find_next_btn.on_click(move |_| find(true));
    }
    {
        let find = do_find.clone();
        find_prev_btn.on_click(move |_| find(false));
    }
    {
        let find = do_find.clone();
        find_ctrl.on_text_enter(move |_| find(true));
    }
    // Ctrl+F focuses the find box with its text selected; F3 /
    // Shift+F3 repeat the search; Escape closes (Python binds
    // the same keys on its help window).
    dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
        let code = ev.get_key_code().unwrap_or(0);
        if code == 27 {
            dialog.end_modal(5100);
            ev.skip(false);
            return;
        }
        let mods = blindrss_core::shortcuts::KeyModifiers {
            ctrl: ev.control_down(),
            alt: ev.alt_down(),
            shift: ev.shift_down(),
            cmd: ev.meta_down(),
        };
        match blindrss_core::shortcuts::event_accel(code, mods, true).as_deref() {
            Some("Ctrl+F") => {
                find_ctrl.set_focus();
                find_ctrl.select_all();
                ev.skip(false);
            }
            Some("F3") => {
                do_find(true);
                ev.skip(false);
            }
            Some("Shift+F3") => {
                do_find(false);
                ev.skip(false);
            }
            _ => ev.skip(true),
        }
    });
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}
