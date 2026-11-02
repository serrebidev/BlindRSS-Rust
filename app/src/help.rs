//! Help viewer dialog: topic list + plain-text reader over the
//! shipped Markdown guide (docs/help/<locale>.md).

use std::path::PathBuf;
use wxdragon::prelude::*;

pub fn help_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BLINDRSS_HELP") {
        return Some(PathBuf::from(p));
    }
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/help"),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("docs/help")))
            .unwrap_or_default(),
    ];
    candidates.into_iter().find(|p| p.join("en.md").exists())
}

pub fn show_help(frame: &Frame, locale: &str) {
    show_help_topic(frame, locale, None);
}

/// Help opened at a specific guide section (F1 context help);
/// unknown ids land on the first topic.
pub fn show_help_topic(frame: &Frame, locale: &str, topic_id: Option<&str>) {
    let Some(dir) = help_dir() else {
        let md = MessageDialog::builder(frame, "Help files were not found.", "Help")
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
        md.show_modal();
        return;
    };
    let guide = blindrss_core::help::load_guide(&dir, locale).unwrap_or_default();
    let topics = blindrss_core::help::topics(&guide);
    let dialog = Dialog::builder(frame, "BlindRSS Help").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let list = ListBox::builder(&dialog).build();
    for topic in &topics {
        list.append(&topic.title);
    }
    let reader = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    let start = topic_id
        .and_then(|id| topics.iter().position(|t| t.id == id))
        .unwrap_or(0);
    if let Some(topic) = topics.get(start) {
        reader.set_value(&blindrss_core::help::to_plain_text(&topic.body));
        list.set_selection(start as u32, true);
    }
    row.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 1, SizerFlag::Expand | SizerFlag::All, 2);
    let close_btn = Button::builder(&dialog).with_label("Close").build();
    sizer.add(&close_btn, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(760, 520));

    let bodies: Vec<String> = topics
        .iter()
        .map(|t| blindrss_core::help::to_plain_text(&t.body))
        .collect();
    list.on_selection_changed(move |e| {
        if let Some(sel) = e.get_selection()
            && let Some(body) = bodies.get(sel as usize)
        {
            reader.set_value(body);
        }
    });
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}
