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
    selected_article: Option<usize>,
    refreshing: bool,
}

enum RefreshMsg {
    FeedDone(Feed, Result<ParsedFeed, String>),
    AllDone,
    /// Result of adding a feed by URL: (url, fetch result).
    Added(String, Result<ParsedFeed, String>),
    /// Result of a hosted-provider sync: (provider name, (feeds, new)).
    ProviderDone(String, Result<(usize, usize), String>),
    /// Fetched article page HTML for full-text extraction.
    FullText(String, Result<String, String>),
}

const ID_OK: i32 = 5100;
const ID_YES: i32 = 5103;

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

        let add_btn = Button::builder(&frame).with_label("Add Feed...").build();
        let remove_btn = Button::builder(&frame).with_label("Remove Feed").build();
        let import_btn = Button::builder(&frame).with_label("Import OPML...").build();
        let export_btn = Button::builder(&frame).with_label("Export OPML...").build();
        let delete_btn = Button::builder(&frame).with_label("Delete Article").build();
        let fav_btn = Button::builder(&frame)
            .with_label("Toggle Favorite")
            .build();
        let markall_btn = Button::builder(&frame).with_label("Mark All Read").build();
        let deleted_btn = Button::builder(&frame).with_label("Deleted...").build();
        let search_box = TextCtrl::builder(&frame).build();
        let search_btn = Button::builder(&frame).with_label("Search").build();
        let sync_btn = Button::builder(&frame).with_label("Sync Providers").build();
        let fulltext_btn = Button::builder(&frame).with_label("Read Full Text").build();
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
        let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
        btn_row.add(&refresh_btn, 0, SizerFlag::All, 4);
        btn_row.add(&add_btn, 0, SizerFlag::All, 4);
        btn_row.add(&remove_btn, 0, SizerFlag::All, 4);
        btn_row.add(&import_btn, 0, SizerFlag::All, 4);
        btn_row.add(&export_btn, 0, SizerFlag::All, 4);
        btn_row.add(&sync_btn, 0, SizerFlag::All, 4);
        btn_row.add(&fulltext_btn, 0, SizerFlag::All, 4);
        sizer.add_sizer(&btn_row, 0, SizerFlag::All, 0);
        let btn_row2 = BoxSizer::builder(Orientation::Horizontal).build();
        btn_row2.add(&delete_btn, 0, SizerFlag::All, 4);
        btn_row2.add(&fav_btn, 0, SizerFlag::All, 4);
        btn_row2.add(&markall_btn, 0, SizerFlag::All, 4);
        btn_row2.add(&deleted_btn, 0, SizerFlag::All, 4);
        btn_row2.add(&search_box, 1, SizerFlag::All, 4);
        btn_row2.add(&search_btn, 0, SizerFlag::All, 4);
        sizer.add_sizer(&btn_row2, 0, SizerFlag::All, 0);
        sizer.add(&splitter, 1, SizerFlag::Expand | SizerFlag::All, 0);
        frame.set_sizer(sizer, true);

        let state = Rc::new(RefCell::new(State {
            db: None,
            feeds: Vec::new(),
            articles: Vec::new(),
            selected_feed: None,
            selected_article: None,
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
            st.selected_article = Some(idx as usize);
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
        let tx_refresh = tx.clone();
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
            let tx = tx_refresh.clone();
            std::thread::spawn(move || {
                for feed in feeds {
                    let result = fetch_and_parse(&feed.url);
                    let _ = tx.send(RefreshMsg::FeedDone(feed, result));
                }
                let _ = tx.send(RefreshMsg::AllDone);
            });
        });

        // Article actions: delete (tombstoned), favorite, mark all
        // read, restore-deleted dialog, and search.
        let state_del = state.clone();
        let frame_del = frame;
        delete_btn.on_click(move |_| {
            let mut st = state_del.borrow_mut();
            let Some(idx) = st.selected_article else {
                frame_del.set_status_text("Select an article first", 0);
                return;
            };
            let Some(a) = st.articles.get(idx).cloned() else {
                return;
            };
            if let Some(db) = st.db.as_ref() {
                let _ = db.delete_article(&a.id);
            }
            st.selected_article = None;
            refill_articles(&mut st, &list);
            frame_del.set_status_text(&format!("Deleted {}", a.title), 0);
        });

        let state_fav = state.clone();
        let frame_fav = frame;
        fav_btn.on_click(move |_| {
            let mut st = state_fav.borrow_mut();
            let Some(idx) = st.selected_article else {
                frame_fav.set_status_text("Select an article first", 0);
                return;
            };
            let Some(a) = st.articles.get(idx).cloned() else {
                return;
            };
            let new_value = !a.is_favorite;
            if let Some(db) = st.db.as_ref() {
                let _ = db.set_favorite(&a.id, new_value);
            }
            if let Some(slot) = st.articles.get_mut(idx) {
                slot.is_favorite = new_value;
            }
            frame_fav.set_status_text(
                &format!(
                    "{} {}",
                    if new_value {
                        "Favorited"
                    } else {
                        "Unfavorited"
                    },
                    a.title
                ),
                0,
            );
        });

        let state_ma = state.clone();
        let frame_ma = frame;
        markall_btn.on_click(move |_| {
            let mut st = state_ma.borrow_mut();
            let filter = st.selected_feed.clone();
            let mut count = 0;
            if let Some(db) = st.db.as_ref() {
                count = db.mark_all_read(filter.as_deref()).unwrap_or(0);
                for a in st.articles.iter_mut() {
                    a.is_read = true;
                }
            }
            frame_ma.set_status_text(&format!("Marked {count} articles read"), 0);
        });

        let state_s = state.clone();
        let frame_s = frame;
        search_btn.on_click(move |_| {
            let query = search_box.get_value();
            let mut st = state_s.borrow_mut();
            if query.trim().is_empty() {
                refill_articles(&mut st, &list);
                return;
            }
            let Some(db) = st.db.as_ref() else { return };
            match db.search_articles(query.trim(), 300) {
                Ok(results) => {
                    list.clear_all();
                    list.insert_column(0, "Title", ListColumnFormat::Left, 420);
                    list.insert_column(1, "Date", ListColumnFormat::Left, 160);
                    list.insert_column(2, "Feed", ListColumnFormat::Left, 160);
                    for (i, a) in results.iter().enumerate() {
                        list.insert_item(i as i64, &a.title, None);
                        list.set_item_text_by_column(i as i64, 1, a.date.as_deref().unwrap_or(""));
                        list.set_item_text_by_column(i as i64, 2, &a.feed_id);
                    }
                    let n = results.len();
                    st.articles = results;
                    st.selected_article = None;
                    frame_s.set_status_text(&format!("{n} matches"), 0);
                }
                Err(e) => frame_s.set_status_text(&format!("Search failed: {e}"), 0),
            }
        });

        let state_deleted = state.clone();
        let frame_deleted = frame;
        deleted_btn.on_click(move |_| {
            let deleted = {
                let st = state_deleted.borrow();
                let Some(db) = st.db.as_ref() else { return };
                db.deleted_articles().unwrap_or_default()
            };
            if deleted.is_empty() {
                frame_deleted.set_status_text("No deleted articles", 0);
                return;
            }
            let dlg = Dialog::builder(&frame_deleted, "Deleted Articles").build();
            dlg.set_size(Size::new(560, 380));
            let lb = ListBox::builder(&dlg).build();
            for a in &deleted {
                lb.append(&a.title);
            }
            let restore_btn = Button::builder(&dlg).with_label("Restore Selected").build();
            let close_btn = Button::builder(&dlg).with_label("Close").build();
            let ds = BoxSizer::builder(Orientation::Vertical).build();
            ds.add(&lb, 1, SizerFlag::Expand | SizerFlag::All, 4);
            let drow = BoxSizer::builder(Orientation::Horizontal).build();
            drow.add(&restore_btn, 0, SizerFlag::All, 4);
            drow.add(&close_btn, 0, SizerFlag::All, 4);
            ds.add_sizer(&drow, 0, SizerFlag::All, 0);
            dlg.set_sizer(ds, true);
            let ids: Vec<String> = deleted.iter().map(|a| a.id.clone()).collect();
            let state_r = state_deleted.clone();
            restore_btn.on_click(move |_| {
                let Some(sel) = lb.get_selection() else {
                    return;
                };
                let Some(id) = ids.get(sel as usize) else {
                    return;
                };
                let mut st = state_r.borrow_mut();
                if let Some(db) = st.db.as_ref() {
                    let _ = db.restore_article(id);
                }
                lb.delete(sel);
                refill_articles(&mut st, &list);
                frame_deleted.set_status_text("Article restored", 0);
            });
            close_btn.on_click(move |_| {
                dlg.end_modal(ID_OK);
            });
            dlg.show_modal();
        });

        // Sync Providers: for each hosted provider configured in
        // config.json, run its sync on a worker thread with its own
        // database connection (SQLite allows concurrent connections;
        // the main thread reloads when the result lands).
        let frame_sync = frame;
        let tx_sync = tx.clone();
        sync_btn.on_click(move |_| {
            let cfg_path = std::env::var("BLINDRSS_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    db_path()
                        .parent()
                        .map(|p| p.join("config.json"))
                        .unwrap_or_else(|| PathBuf::from("config.json"))
                });
            let cfg = blindrss_core::config::Config::load(&cfg_path).unwrap_or_default();
            let providers = cfg.raw().get("providers").cloned();
            let Some(providers) = providers else {
                frame_sync.set_status_text("No providers configured", 0);
                return;
            };
            let get = |obj: &serde_json::Value, key: &str| -> String {
                obj.get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let mut started = 0;
            for (key, service) in [
                (
                    "theoldreader",
                    blindrss_core::providers::GReaderService::TheOldReader,
                ),
                ("bazqux", blindrss_core::providers::GReaderService::BazQux),
            ] {
                let Some(section) = providers.get(key) else {
                    continue;
                };
                let (email, password) = (get(section, "email"), get(section, "password"));
                if email.is_empty() || password.is_empty() {
                    continue;
                }
                started += 1;
                let tx = tx_sync.clone();
                let path = db_path();
                std::thread::spawn(move || {
                    let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                        blindrss_core::providers::GReaderClient::new(service, &email, &password)
                            .sync(&db, 100)
                    });
                    let _ = tx.send(RefreshMsg::ProviderDone(key.to_string(), result));
                });
            }
            if let Some(section) = providers.get("inoreader") {
                let token = get(section, "token");
                if !token.is_empty() {
                    started += 1;
                    let tx = tx_sync.clone();
                    let path = db_path();
                    std::thread::spawn(move || {
                        let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                            blindrss_core::providers::GReaderClient::with_oauth(
                                blindrss_core::providers::GReaderService::Inoreader,
                                &token,
                            )
                            .sync(&db, 100)
                        });
                        let _ = tx.send(RefreshMsg::ProviderDone("inoreader".to_string(), result));
                    });
                }
            }
            if let Some(section) = providers.get("miniflux") {
                let (url, api_key) = (get(section, "url"), get(section, "api_key"));
                if !url.is_empty() && !api_key.is_empty() {
                    started += 1;
                    let tx = tx_sync.clone();
                    let path = db_path();
                    std::thread::spawn(move || {
                        let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                            blindrss_core::providers::MinifluxClient::new(&url, &api_key)
                                .sync(&db, 100)
                        });
                        let _ = tx.send(RefreshMsg::ProviderDone("miniflux".to_string(), result));
                    });
                }
            }
            if started == 0 {
                frame_sync.set_status_text("No providers configured", 0);
            } else {
                frame_sync.set_status_text(&format!("Syncing {started} provider(s)..."), 0);
            }
        });

        // Read Full Text: fetch the selected article's page on a
        // worker thread; extraction runs on the main thread when the
        // HTML lands (feed body is the fallback, per the cascade).
        let state_ft = state.clone();
        let frame_ft = frame;
        let tx_ft = tx.clone();
        fulltext_btn.on_click(move |_| {
            let st = state_ft.borrow();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_ft.set_status_text("No article selected", 0);
                return;
            };
            if article.url.is_empty() {
                frame_ft.set_status_text("Article has no link", 0);
                return;
            }
            frame_ft.set_status_text("Fetching full text...", 0);
            let tx = tx_ft.clone();
            let id = article.id.clone();
            let url = article.url.clone();
            std::thread::spawn(move || {
                let result = blindrss_core::extract::fetch_html(&url);
                let _ = tx.send(RefreshMsg::FullText(id, result));
            });
        });

        // Add Feed: prompt for a URL, fetch it on a worker thread to
        // learn its title, then subscribe + store its current articles.
        let state_add = state.clone();
        let frame_add = frame;
        let tx_add = tx.clone();
        add_btn.on_click(move |_| {
            let dlg = TextEntryDialog::builder(&frame_add, "Feed URL:", "Add Feed").build();
            if dlg.show_modal() != ID_OK {
                return;
            }
            let Some(url) = dlg.get_value().filter(|v| !v.trim().is_empty()) else {
                return;
            };
            let url = url.trim().to_string();
            {
                let st = state_add.borrow();
                if st.feeds.iter().any(|f| f.url == url) {
                    frame_add.set_status_text("That feed is already subscribed", 0);
                    return;
                }
            }
            frame_add.set_status_text(&format!("Adding {url}..."), 0);
            let tx = tx_add.clone();
            std::thread::spawn(move || {
                let result = fetch_and_parse(&url);
                let _ = tx.send(RefreshMsg::Added(url, result));
            });
        });

        // Remove Feed: removes the feed selected in the tree, after a
        // confirmation (it also deletes that feed's stored articles).
        let state_rm = state.clone();
        let frame_rm = frame;
        remove_btn.on_click(move |_| {
            let feed = {
                let st = state_rm.borrow();
                let Some(id) = st.selected_feed.clone() else {
                    frame_rm.set_status_text("Select a feed in the tree first", 0);
                    return;
                };
                st.feeds.iter().find(|f| f.id == id).cloned()
            };
            let Some(feed) = feed else { return };
            let dlg = MessageDialog::builder(
                &frame_rm,
                &format!("Remove {} and its stored articles?", feed.title),
                "Remove Feed",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build();
            if dlg.show_modal() != ID_YES {
                return;
            }
            let mut st = state_rm.borrow_mut();
            if let Some(db) = st.db.as_ref() {
                let _ = db.remove_feed(&feed.id);
            }
            st.feeds.retain(|f| f.id != feed.id);
            st.selected_feed = None;
            rebuild_tree(&mut st, &tree);
            refill_articles(&mut st, &list);
            frame_rm.set_status_text(&format!("Removed {}", feed.title), 0);
        });

        // OPML import/export.
        let state_imp = state.clone();
        let frame_imp = frame;
        import_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_imp)
                .with_message("Import OPML")
                .with_wildcard("OPML files (*.opml;*.xml)|*.opml;*.xml")
                .with_style(FileDialogStyle::Open)
                .build();
            if dlg.show_modal() != ID_OK {
                return;
            }
            let Some(path) = dlg.get_path() else { return };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    frame_imp.set_status_text(&format!("Could not read file: {e}"), 0);
                    return;
                }
            };
            let mut st = state_imp.borrow_mut();
            let Some(db) = st.db.as_ref() else { return };
            match blindrss_core::opml::import_opml(db, &text) {
                Ok(n) => {
                    st.feeds = db.feeds().unwrap_or_default();
                    rebuild_tree(&mut st, &tree);
                    refill_articles(&mut st, &list);
                    frame_imp.set_status_text(&format!("Imported {n} feeds"), 0);
                }
                Err(e) => frame_imp.set_status_text(&format!("Import failed: {e}"), 0),
            }
        });

        let state_exp = state.clone();
        let frame_exp = frame;
        export_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_exp)
                .with_message("Export OPML")
                .with_wildcard("OPML files (*.opml)|*.opml")
                .with_style(FileDialogStyle::Save)
                .build();
            if dlg.show_modal() != ID_OK {
                return;
            }
            let Some(path) = dlg.get_path() else { return };
            let st = state_exp.borrow();
            let Some(db) = st.db.as_ref() else { return };
            match blindrss_core::opml::export_opml(db) {
                Ok(xml) => match std::fs::write(&path, xml) {
                    Ok(()) => frame_exp.set_status_text(&format!("Exported to {path}"), 0),
                    Err(e) => frame_exp.set_status_text(&format!("Could not write file: {e}"), 0),
                },
                Err(e) => frame_exp.set_status_text(&format!("Export failed: {e}"), 0),
            }
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
                    Ok(RefreshMsg::Added(url, Ok(parsed))) => {
                        let mut st = state_tick.borrow_mut();
                        if let Some(db) = st.db.as_ref() {
                            let title = if parsed.title.trim().is_empty() {
                                url.clone()
                            } else {
                                parsed.title.clone()
                            };
                            match db.add_feed(&url, &title, None) {
                                Ok(feed) => {
                                    let _ = store_parsed(db, &feed, &parsed);
                                    st.feeds.push(feed);
                                    rebuild_tree(&mut st, &tree);
                                    refill_articles(&mut st, &list);
                                    frame_tick.set_status_text(&format!("Added {title}"), 0);
                                }
                                Err(e) => {
                                    frame_tick.set_status_text(&format!("Add failed: {e}"), 0)
                                }
                            }
                        }
                    }
                    Ok(RefreshMsg::Added(url, Err(e))) => {
                        frame_tick.set_status_text(&format!("Could not add {url}: {e}"), 0);
                    }
                    Ok(RefreshMsg::ProviderDone(name, result)) => {
                        let mut st = state_tick.borrow_mut();
                        match result {
                            Ok((feeds, new)) => {
                                st.feeds = st
                                    .db
                                    .as_ref()
                                    .and_then(|db| db.feeds().ok())
                                    .unwrap_or_default();
                                rebuild_tree(&mut st, &tree);
                                refill_articles(&mut st, &list);
                                frame_tick.set_status_text(
                                    &format!("{name}: {feeds} feeds, {new} new articles"),
                                    0,
                                );
                            }
                            Err(e) => frame_tick.set_status_text(&format!("{name}: {e}"), 0),
                        }
                    }
                    Ok(RefreshMsg::FullText(id, result)) => {
                        let st = state_tick.borrow();
                        let article = st.articles.iter().find(|a| a.id == id).cloned();
                        match (article, result) {
                            (Some(article), Ok(html)) => {
                                let outcome = blindrss_core::extract::extract_article(
                                    &html,
                                    &article.url,
                                    article.content.as_deref(),
                                );
                                use blindrss_core::extract::ExtractOutcome as EO;
                                match outcome {
                                    EO::Article { text, source, .. } => {
                                        reader.set_value(&format!("{}\n\n{}", article.title, text));
                                        frame_tick
                                            .set_status_text(&format!("Full text ({source})"), 0);
                                    }
                                    EO::Gate { reason } => {
                                        frame_tick.set_status_text(
                                            &format!("Full text unavailable: {reason}"),
                                            0,
                                        );
                                    }
                                    EO::Empty => {
                                        frame_tick.set_status_text(
                                            "Full text unavailable: nothing extracted",
                                            0,
                                        );
                                    }
                                }
                            }
                            (_, Err(e)) => {
                                frame_tick.set_status_text(&format!("Full text: {e}"), 0)
                            }
                            _ => {}
                        }
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
