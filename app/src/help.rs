//! Help viewer dialog: the complete guide in ONE read-only
//! text control (continuous reading across topics), a contents
//! list that jumps by character offset, and Find with
//! wrapping (Audit 8 S07/S08; Python gui/help_viewer.py over
//! core/help_docs.py). The guide shown is the locale fallback
//! chain regional → base → English; when the English text is
//! shown for another requested language, a notice at the top
//! says so, as Python's viewer does.
//!
//! The changelog is NOT shown in this viewer: it is an HTML
//! page (converted from CHANGELOG.md at build time) that
//! opens in the default browser — the BlindRadio pattern.

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

/// The changelog as an HTML page, embedded deflated by
/// app/build.rs (shipfitter::docs over the repo-root
/// CHANGELOG.md), so it reads fully offline.
const CHANGELOG_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/changelog.html"));

fn inflate(page: &[u8]) -> String {
    use std::io::Read;
    let mut html = String::new();
    flate2::read::DeflateDecoder::new(page)
        .read_to_string(&mut html)
        .expect("an embedded page inflates");
    html
}

/// Writes a page into the temp folder and opens it in the
/// default browser (BlindRadio's `help::open`).
fn open_page(html: &str, name: &str) -> bool {
    let dir = std::env::temp_dir().join("BlindRSS");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = dir.join(name);
    if std::fs::write(&path, html).is_err() {
        return false;
    }
    launch_default_application(&path.to_string_lossy())
}

/// Help > View Changelog: the FULL release history — every
/// version, including the ones an update skipped over — as
/// one page in the default browser (Brandon's call,
/// 2026-11-13: the BlindRadio approach). The in-app
/// two-pane viewer was needlessly complicated next to the
/// browser, where reading, Find and links already behave
/// the way the user knows.
pub fn show_changelog(_frame: &Frame) {
    if !open_page(&inflate(CHANGELOG_HTML), "changelog.html") {
        crate::announce("general", &t("Could not open the browser"));
    }
}

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
    crate::dialogs::escape_closes(&dialog);
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
    // The guide keeps its own builder because its position
    // starts mid-document at a topic line.
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
    let close_btn = Button::builder(&dialog)
        .with_id(ID_CANCEL)
        .with_label(&t("Close"))
        .build();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_page_embedded_with_this_version() {
        let html = inflate(CHANGELOG_HTML);
        assert!(
            html.contains(env!("CARGO_PKG_VERSION")),
            "the embedded changelog page lacks this version"
        );
        assert!(
            html.contains("<title>Changelog</title>"),
            "the embedded page lost its title"
        );
    }
}
