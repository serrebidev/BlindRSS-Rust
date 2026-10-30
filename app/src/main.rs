//! BlindRSS (Rust) — Stage 0/4 slice: main window over the existing
//! Python database. Feed tree, article list, native text reader.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

use blindrss_core::{Article, Db};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BLINDRSS_DB") {
        return PathBuf::from(p);
    }
    // Dev default: the Python checkout's rss.db beside this workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../blindrss-work/rss.db")
}

struct State {
    articles: Vec<Article>,
}

fn main() {
    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("BlindRSS")
            .with_size(Size::new(1100, 700))
            .build();
        frame.create_status_bar(1, 0, -1, "status");

        let splitter = SplitterWindow::builder(&frame).build();
        let tree = TreeCtrl::builder(&splitter).build();
        let right = Panel::builder(&splitter).build();
        let list = ListCtrl::builder(&right)
            .with_style(ListCtrlStyle::Report)
            .build();
        list.insert_column(0, "Title", ListColumnFormat::Left, 420);
        list.insert_column(1, "Date", ListColumnFormat::Left, 160);
        list.insert_column(2, "Feed", ListColumnFormat::Left, 160);
        let reader = TextCtrl::builder(&right)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
            .build();

        let right_sizer = BoxSizer::builder(Orientation::Vertical).build();
        right_sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 2);
        right_sizer.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 2);
        right.set_sizer(right_sizer, true);
        splitter.split_vertically(&tree, &right, 280);

        let sizer = BoxSizer::builder(Orientation::Vertical).build();
        sizer.add(&splitter, 1, SizerFlag::Expand | SizerFlag::All, 0);
        frame.set_sizer(sizer, true);

        let state = Rc::new(RefCell::new(State {
            articles: Vec::new(),
        }));

        // Load feeds + articles from the existing database.
        let path = db_path();
        match Db::open(&path) {
            Ok(db) => {
                if let Some(root) = tree.add_root("Feeds", None, None) {
                    if let Ok(feeds) = db.feeds() {
                        for f in &feeds {
                            let label = match db.unread_count(Some(&f.id)) {
                                Ok(n) if n > 0 => format!("{} ({n})", f.title),
                                _ => f.title.clone(),
                            };
                            tree.append_item(&root, &label, None, None);
                        }
                    }
                    tree.expand(&root);
                }
                if let Ok(articles) = db.articles(None, 200) {
                    for (i, a) in articles.iter().enumerate() {
                        list.insert_item(i as i64, &a.title, None);
                        list.set_item_text_by_column(i as i64, 1, a.date.as_deref().unwrap_or(""));
                        list.set_item_text_by_column(i as i64, 2, &a.feed_id);
                    }
                    state.borrow_mut().articles = articles;
                }
                frame.set_status_text(&format!("Loaded {}", path.display()), 0);
            }
            Err(e) => {
                frame.set_status_text(&format!("Could not open database: {e}"), 0);
            }
        }

        // Reader: show the selected article's stored text.
        let reader_c = reader;
        let state_c = state.clone();
        list.on_item_selected(move |ev| {
            let idx = ev.get_item_index();
            if idx < 0 {
                return;
            }
            let st = state_c.borrow();
            if let Some(a) = st.articles.get(idx as usize) {
                let body = a
                    .content
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .or_else(|| a.description.clone())
                    .unwrap_or_else(|| "(no stored text)".to_string());
                reader_c.set_value(&format!("{}\n\n{}", a.title, body));
            }
        });

        frame.show(true);
        frame.centre();
    });
}
