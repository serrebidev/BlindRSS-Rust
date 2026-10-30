//! BlindRSS (Rust) — main window over the existing Python database.
//! Slice 2: Refresh All (worker-thread fetch, main-thread store),
//! feed filtering via the tree, mark-read on open.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

use blindrss_core::feeds::fetch_and_parse;
use blindrss_core::{Article, Db, Feed, ParsedFeed, store_parsed};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use wxdragon::prelude::*;

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BLINDRSS_DB") {
        return PathBuf::from(p);
    }
    // Dev default: the Python checkout's rss.db beside this workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../blindrss-work/rss.db")
}

struct State {
    db: Option<Db>,
    feeds: Vec<Feed>,
    articles: Vec<Article>,
    selected_feed: Option<String>,
    refreshing: bool,
}

enum RefreshMsg {
    FeedDone(Feed, Result<ParsedFeed, String>),
    AllDone,
}

fn feed_label(db: &Db, feed: &Feed) -> String {
    match db.unread_count(Some(&feed.id)) {
        Ok(n) if n > 0 => format!("{} ({n})", feed.title),
        _ => feed.title.clone(),
    }
}

fn rebuild_tree(state: &mut State, tree: &TreeCtrl) {
    tree.delete_all_items();
    let Some(root) = tree.add_root("Feeds", None, None) else {
        return;
    };
    // Item data carries the feed filter: empty string = all feeds,
    // otherwise the feed id. (TreeItemId has no PartialEq, so data is
    // the reliable way to identify the selected item.)
    tree.set_custom_data_direct(&root, String::new());
    let Some(db) = state.db.as_ref() else { return };
    let feeds = state.feeds.clone();
    for f in &feeds {
        let label = feed_label(db, f);
        tree.append_item_with_data(&root, &label, f.id.clone(), None, None);
    }
    tree.expand(&root);
}

fn refill_articles(state: &mut State, list: &ListCtrl) {
    list.clear_all();
    list.insert_column(0, "Title", ListColumnFormat::Left, 420);
    list.insert_column(1, "Date", ListColumnFormat::Left, 160);
    list.insert_column(2, "Feed", ListColumnFormat::Left, 160);
    state.articles.clear();
    let Some(db) = state.db.as_ref() else { return };
    let filter = state.selected_feed.as_deref();
    if let Ok(articles) = db.articles(filter, 300) {
        for (i, a) in articles.iter().enumerate() {
            list.insert_item(i as i64, &a.title, None);
            list.set_item_text_by_column(i as i64, 1, a.date.as_deref().unwrap_or(""));
            list.set_item_text_by_column(i as i64, 2, &a.feed_id);
        }
        state.articles = articles;
    }
}

fn main() {
    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("BlindRSS")
            .with_size(Size::new(1100, 700))
            .build();
        frame.create_status_bar(1, 0, -1, "status");

        let refresh_btn = Button::builder(&frame)
            .with_label("Refresh All Feeds")
            .build();

        let splitter = SplitterWindow::builder(&frame).build();
        let tree = TreeCtrl::builder(&splitter).build();
        let right = Panel::builder(&splitter).build();
        let list = ListCtrl::builder(&right)
            .with_style(ListCtrlStyle::Report)
            .build();
        let reader = TextCtrl::builder(&right)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
            .build();

        let right_sizer = BoxSizer::builder(Orientation::Vertical).build();
        right_sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 2);
        right_sizer.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 2);
        right.set_sizer(right_sizer, true);
        splitter.split_vertically(&tree, &right, 280);

        let sizer = BoxSizer::builder(Orientation::Vertical).build();
        sizer.add(&refresh_btn, 0, SizerFlag::All, 4);
        sizer.add(&splitter, 1, SizerFlag::Expand | SizerFlag::All, 0);
        frame.set_sizer(sizer, true);

        let state = Rc::new(RefCell::new(State {
            db: None,
            feeds: Vec::new(),
            articles: Vec::new(),
            selected_feed: None,
            refreshing: false,
        }));

        // Open the existing database and populate.
        let path = db_path();
        {
            let mut st = state.borrow_mut();
            match Db::open(&path) {
                Ok(db) => {
                    st.feeds = db.feeds().unwrap_or_default();
                    st.db = Some(db);
                    frame.set_status_text(&format!("Loaded {}", path.display()), 0);
                }
                Err(e) => {
                    frame.set_status_text(&format!("Could not open database: {e}"), 0);
                }
            }
        }
        {
            let mut st = state.borrow_mut();
            rebuild_tree(&mut st, &tree);
            refill_articles(&mut st, &list);
        }

        // Tree selection filters the article list.
        let state_tree = state.clone();
        tree.on_selection_changed(move |ev| {
            let Some(item) = ev.get_item() else { return };
            let filter = tree
                .get_custom_data_direct(&item)
                .and_then(|d| d.downcast_ref::<String>().cloned())
                .filter(|s| !s.is_empty());
            let mut st = state_tree.borrow_mut();
            st.selected_feed = filter;
            refill_articles(&mut st, &list);
        });

        // Selecting an article shows it and marks it read (Python marks
        // read on open as well).
        let state_list = state.clone();
        let frame_list = frame;
        list.on_item_selected(move |ev| {
            let idx = ev.get_item_index();
            if idx < 0 {
                return;
            }
            let mut st = state_list.borrow_mut();
            let Some(a) = st.articles.get(idx as usize).cloned() else {
                return;
            };
            let body = a
                .content
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| a.description.clone())
                .unwrap_or_else(|| "(no stored text)".to_string());
            reader.set_value(&format!("{}\n\n{}", a.title, body));
            if !a.is_read {
                if let Some(db) = st.db.as_ref() {
                    let _ = db.mark_read(&a.id, true);
                }
                if let Some(slot) = st.articles.get_mut(idx as usize) {
                    slot.is_read = true;
                }
                frame_list.set_status_text(&format!("Read: {}", a.title), 0);
            }
        });

        // Refresh: worker threads fetch + parse; results return over a
        // channel; a timer drains them on the main thread, which owns
        // the database connection.
        let (tx, rx): (Sender<RefreshMsg>, Receiver<RefreshMsg>) = channel();
        let rx = Rc::new(RefCell::new(rx));
        let state_btn = state.clone();
        let frame_btn = frame;
        refresh_btn.on_click(move |_| {
            let feeds = {
                let mut st = state_btn.borrow_mut();
                if st.refreshing {
                    return;
                }
                st.refreshing = true;
                st.feeds.clone()
            };
            frame_btn.set_status_text(&format!("Refreshing {} feeds...", feeds.len()), 0);
            let tx = tx.clone();
            std::thread::spawn(move || {
                for feed in feeds {
                    let result = fetch_and_parse(&feed.url);
                    let _ = tx.send(RefreshMsg::FeedDone(feed, result));
                }
                let _ = tx.send(RefreshMsg::AllDone);
            });
        });

        let timer = Box::leak(Box::new(Timer::new(&frame)));
        let state_tick = state.clone();
        let frame_tick = frame;
        let rx_tick = rx.clone();
        timer.on_tick(move |_| {
            let mut new_total = 0usize;
            let mut finished = false;
            loop {
                let msg = rx_tick.borrow().try_recv();
                match msg {
                    Ok(RefreshMsg::FeedDone(feed, Ok(parsed))) => {
                        let st = state_tick.borrow();
                        if let Some(db) = st.db.as_ref() {
                            match store_parsed(db, &feed, &parsed) {
                                Ok(n) => new_total += n,
                                Err(e) => frame_tick.set_status_text(
                                    &format!("{}: store failed: {e}", feed.title),
                                    0,
                                ),
                            }
                        }
                    }
                    Ok(RefreshMsg::FeedDone(feed, Err(e))) => {
                        frame_tick.set_status_text(&format!("{}: {e}", feed.title), 0);
                    }
                    Ok(RefreshMsg::AllDone) => {
                        finished = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if finished {
                let mut st = state_tick.borrow_mut();
                st.refreshing = false;
                rebuild_tree(&mut st, &tree);
                refill_articles(&mut st, &list);
                frame_tick
                    .set_status_text(&format!("Refresh complete: {new_total} new articles"), 0);
            }
        });
        timer.start(150, false);

        frame.show(true);
        frame.centre();
    });
}
