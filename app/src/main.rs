//! BlindRSS (Rust) — main window over the existing Python database.
//! Slice 2: Refresh All (worker-thread fetch, main-thread store),
//! feed filtering via the tree, mark-read on open.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

mod player;

use blindrss_core::feeds::fetch_and_parse;
use blindrss_core::{Article, Db, Feed, ParsedFeed, store_parsed};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use wxdragon::prelude::*;

fn config_path() -> PathBuf {
    std::env::var("BLINDRSS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            db_path()
                .parent()
                .map(|p| p.join("config.json"))
                .unwrap_or_else(|| PathBuf::from("config.json"))
        })
}

/// Best-effort remote read-state propagation for hosted-provider
/// articles (Python providers push mark_read/mark_unread to the
/// server the same way). Runs on a worker thread; failures are
/// silent because the local state is already correct.
fn propagate_read_state(article: &blindrss_core::db::Article, read: bool) {
    let article = article.clone();
    std::thread::spawn(move || {
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        let Some(providers) = cfg.raw().get("providers").cloned() else {
            return;
        };
        let get = |obj: &serde_json::Value, key: &str| -> String {
            obj.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        if article.feed_id.starts_with("miniflux:") {
            if let Some(section) = providers.get("miniflux") {
                let (url, key) = (get(section, "url"), get(section, "api_key"));
                if let Some(entry) = article
                    .id
                    .strip_prefix("miniflux-entry:")
                    .and_then(|s| s.parse::<i64>().ok())
                    && !url.is_empty()
                    && !key.is_empty()
                {
                    let status = if read { "read" } else { "unread" };
                    let _ = blindrss_core::providers::MinifluxClient::new(&url, &key)
                        .set_status_remote(&[entry], status);
                }
            }
            return;
        }
        if article.feed_id.starts_with("feed/") {
            use blindrss_core::providers::{GReaderClient, GReaderService};
            for (key, service) in [
                ("theoldreader", GReaderService::TheOldReader),
                ("bazqux", GReaderService::BazQux),
            ] {
                if let Some(section) = providers.get(key) {
                    let (email, password) = (get(section, "email"), get(section, "password"));
                    if !email.is_empty()
                        && !password.is_empty()
                        && GReaderClient::new(service, &email, &password)
                            .set_read_remote(&article.id, read)
                            .is_ok()
                    {
                        return;
                    }
                }
            }
            if let Some(section) = providers.get("inoreader") {
                let token = get(section, "token");
                if !token.is_empty() {
                    let _ = GReaderClient::with_oauth(GReaderService::Inoreader, &token)
                        .set_read_remote(&article.id, read);
                }
            }
        }
    });
}

/// Modal text prompt; None when cancelled.
fn prompt_text(
    parent: &impl WxWidget,
    message: &str,
    caption: &str,
    default: &str,
) -> Option<String> {
    let dlg = TextEntryDialog::builder(parent, message, caption)
        .with_default_value(default)
        .build();
    if dlg.show_modal() == 5100 {
        dlg.get_value()
    } else {
        None
    }
}

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
    Added(String, Result<(String, ParsedFeed), String>),
    /// Result of a hosted-provider sync: (provider name, (feeds, new)).
    ProviderDone(String, Result<(usize, usize), String>),
    /// Fetched article page HTML for full-text extraction.
    FullText(String, Result<String, String>),
    /// yt-dlp resolution for playback: (stream URL, info if fetched).
    MediaReady(
        String,
        Result<(String, Option<blindrss_core::media::MediaInfo>), String>,
    ),
    /// A finished download: description of what landed where.
    DownloadDone(Result<String, String>),
    /// YouTube full text for an article.
    YoutubeText(String, Result<String, String>),
    /// Archive rescan for a feed: (feed title, new count).
    ArchiveDone(String, Result<usize, String>),
    /// A finished cast attempt.
    CastDone(Result<String, String>),
    /// Update check: latest release tag.
    UpdateChecked(Result<(String, Option<String>), String>),
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
    // Smart folders: item data is "smart:<id>".
    if let Ok(folders) = db.smart_folders()
        && !folders.is_empty()
    {
        let Some(sf_root) =
            tree.append_item_with_data(&root, "Smart Folders", "smart:".to_string(), None, None)
        else {
            return;
        };
        for (id, name, _rule, _pos) in &folders {
            tree.append_item_with_data(&sf_root, name, format!("smart:{id}"), None, None);
        }
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
    // Smart folder selection: evaluate the folder's rule over all
    // articles instead of a feed filter.
    if let Some(smart_id) = filter.and_then(|f| f.strip_prefix("smart:"))
        && !smart_id.is_empty()
    {
        let folders = db.smart_folders().unwrap_or_default();
        if let Some((_, _, rule_json, _)) = folders.iter().find(|(id, ..)| id == smart_id) {
            let rule: serde_json::Value =
                serde_json::from_str(rule_json).unwrap_or(serde_json::Value::Null);
            let feed_titles: std::collections::HashMap<&str, &str> = state
                .feeds
                .iter()
                .map(|f| (f.id.as_str(), f.title.as_str()))
                .collect();
            if let Ok(all) = db.articles(None, 500) {
                let matched: Vec<Article> = all
                    .into_iter()
                    .filter(|a| {
                        let view = blindrss_core::rules::RuleArticle {
                            article: a,
                            feed_title: feed_titles.get(a.feed_id.as_str()).copied().unwrap_or(""),
                            opened: false,
                            updated: false,
                            tags: &[],
                        };
                        blindrss_core::rules::rule_matches(&rule, &view)
                    })
                    .collect();
                for (i, a) in matched.iter().enumerate() {
                    list.insert_item(i as i64, &a.title, None);
                    list.set_item_text_by_column(i as i64, 1, a.date.as_deref().unwrap_or(""));
                    list.set_item_text_by_column(i as i64, 2, &a.feed_id);
                }
                state.articles = matched;
            }
        }
        return;
    }
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
        let toggle_read_btn = Button::builder(&frame).with_label("Toggle Read").build();
        let edit_feed_btn = Button::builder(&frame).with_label("Edit Feed").build();
        let rename_cat_btn = Button::builder(&frame)
            .with_label("Rename Category")
            .build();
        let set_cat_btn = Button::builder(&frame)
            .with_label("Set Feed Category")
            .build();
        let play_btn = Button::builder(&frame).with_label("Play Media").build();
        let new_sf_btn = Button::builder(&frame)
            .with_label("New Smart Folder")
            .build();
        let del_sf_btn = Button::builder(&frame)
            .with_label("Delete Smart Folder")
            .build();
        let download_btn = Button::builder(&frame).with_label("Download").build();
        let takeout_btn = Button::builder(&frame)
            .with_label("Import YouTube Takeout")
            .build();
        let archive_btn = Button::builder(&frame).with_label("Rescan Archive").build();
        let cast_btn = Button::builder(&frame).with_label("Cast").build();
        let backup_btn = Button::builder(&frame).with_label("Backup").build();
        let restore_btn = Button::builder(&frame).with_label("Restore").build();
        let prune_btn = Button::builder(&frame)
            .with_label("Prune Old Articles")
            .build();
        let update_btn = Button::builder(&frame)
            .with_label("Check for Updates")
            .build();
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
        btn_row.add(&toggle_read_btn, 0, SizerFlag::All, 4);
        btn_row.add(&edit_feed_btn, 0, SizerFlag::All, 4);
        btn_row.add(&rename_cat_btn, 0, SizerFlag::All, 4);
        btn_row.add(&set_cat_btn, 0, SizerFlag::All, 4);
        btn_row.add(&play_btn, 0, SizerFlag::All, 4);
        btn_row.add(&new_sf_btn, 0, SizerFlag::All, 4);
        btn_row.add(&del_sf_btn, 0, SizerFlag::All, 4);
        btn_row.add(&download_btn, 0, SizerFlag::All, 4);
        btn_row.add(&takeout_btn, 0, SizerFlag::All, 4);
        btn_row.add(&archive_btn, 0, SizerFlag::All, 4);
        btn_row.add(&cast_btn, 0, SizerFlag::All, 4);
        btn_row.add(&backup_btn, 0, SizerFlag::All, 4);
        btn_row.add(&restore_btn, 0, SizerFlag::All, 4);
        btn_row.add(&prune_btn, 0, SizerFlag::All, 4);
        btn_row.add(&update_btn, 0, SizerFlag::All, 4);
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
                // YouTube articles use the dedicated full-text path
                // (description + chapters + transcript), like Python.
                if url.contains("youtube.com") || url.contains("youtu.be") {
                    let result = blindrss_core::download::youtube_full_text(&url);
                    let _ = tx.send(RefreshMsg::YoutubeText(id, result));
                } else {
                    let jar = blindrss_core::cookies::load_jar_beside(&config_path());
                    let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
                    let result = blindrss_core::extract::fetch_html_with_cookies(&url, &header);
                    let _ = tx.send(RefreshMsg::FullText(id, result));
                }
            });
        });

        // Toggle Read: flips the selected article locally and
        // propagates to the hosted provider when it came from one.
        let state_tr = state.clone();
        let frame_tr = frame;
        let list_tr = list;
        toggle_read_btn.on_click(move |_| {
            let mut st = state_tr.borrow_mut();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_tr.set_status_text("No article selected", 0);
                return;
            };
            let new_state = !article.is_read;
            if let Some(db) = &st.db {
                let _ = db.mark_read(&article.id, new_state);
            }
            if let Some(row) = st.articles.iter_mut().find(|a| a.id == article.id) {
                row.is_read = new_state;
            }
            refill_articles(&mut st, &list_tr);
            frame_tr.set_status_text(
                &format!("Marked {}", if new_state { "read" } else { "unread" }),
                0,
            );
            drop(st);
            propagate_read_state(&article, new_state);
        });

        // Edit Feed: title + URL prompts for the tree-selected feed.
        let state_ef = state.clone();
        let frame_ef = frame;
        let tree_ef = tree;
        let list_ef = list;
        edit_feed_btn.on_click(move |_| {
            let (feed, db_ok) = {
                let st = state_ef.borrow();
                (
                    st.selected_feed
                        .as_ref()
                        .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned()),
                    st.db.is_some(),
                )
            };
            let Some(feed) = feed else {
                frame_ef.set_status_text("Select a feed in the tree first", 0);
                return;
            };
            if !db_ok {
                return;
            }
            let Some(title) = prompt_text(&frame_ef, "Feed title:", "Edit Feed", &feed.title)
            else {
                return;
            };
            let Some(url) = prompt_text(&frame_ef, "Feed URL:", "Edit Feed", &feed.url) else {
                return;
            };
            let mut st = state_ef.borrow_mut();
            if let Some(db) = &st.db {
                let _ = db.update_feed(
                    &feed.id,
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
                st.feeds = db.feeds().unwrap_or_default();
            }
            rebuild_tree(&mut st, &tree_ef);
            refill_articles(&mut st, &list_ef);
            frame_ef.set_status_text("Feed updated", 0);
        });

        // Rename Category: renames the selected feed's category
        // everywhere (category row + all feeds in it).
        let state_rc = state.clone();
        let frame_rc = frame;
        let tree_rc = tree;
        let list_rc = list;
        rename_cat_btn.on_click(move |_| {
            let (category, db_ok) = {
                let st = state_rc.borrow();
                (
                    st.selected_feed.as_ref().and_then(|id| {
                        st.feeds
                            .iter()
                            .find(|f| &f.id == id)
                            .and_then(|f| f.category.clone())
                    }),
                    st.db.is_some(),
                )
            };
            let Some(category) = category else {
                frame_rc.set_status_text("Selected feed has no category", 0);
                return;
            };
            if !db_ok {
                return;
            }
            let Some(new_name) = prompt_text(
                &frame_rc,
                "New category name:",
                "Rename Category",
                &category,
            ) else {
                return;
            };
            if new_name.trim().is_empty() || new_name.trim() == category {
                return;
            }
            let mut st = state_rc.borrow_mut();
            if let Some(db) = &st.db {
                let _ = db.rename_category(&category, new_name.trim());
                st.feeds = db.feeds().unwrap_or_default();
            }
            rebuild_tree(&mut st, &tree_rc);
            refill_articles(&mut st, &list_rc);
            frame_rc.set_status_text("Category renamed", 0);
        });

        // Set Feed Category: prompt (blank = uncategorized); a new
        // name creates the category.
        let state_sc = state.clone();
        let frame_sc = frame;
        let tree_sc = tree;
        let list_sc = list;
        set_cat_btn.on_click(move |_| {
            let (feed, db_ok) = {
                let st = state_sc.borrow();
                (
                    st.selected_feed
                        .as_ref()
                        .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned()),
                    st.db.is_some(),
                )
            };
            let Some(feed) = feed else {
                frame_sc.set_status_text("Select a feed in the tree first", 0);
                return;
            };
            if !db_ok {
                return;
            }
            let Some(name) = prompt_text(
                &frame_sc,
                "Category (blank for none):",
                "Set Feed Category",
                feed.category.as_deref().unwrap_or(""),
            ) else {
                return;
            };
            let name = name.trim();
            let mut st = state_sc.borrow_mut();
            if let Some(db) = &st.db {
                if name.is_empty() {
                    let _ = db.set_feed_category(&feed.id, None);
                } else {
                    let _ = db.add_category(name);
                    let _ = db.set_feed_category(&feed.id, Some(name));
                }
                st.feeds = db.feeds().unwrap_or_default();
            }
            rebuild_tree(&mut st, &tree_sc);
            refill_articles(&mut st, &list_sc);
            frame_sc.set_status_text("Feed category set", 0);
        });

        // Play Media: direct media URL plays immediately; a page
        // URL (YouTube etc.) resolves through yt-dlp on a worker
        // thread first.
        let state_pm = state.clone();
        let frame_pm = frame;
        let tx_pm = tx.clone();
        play_btn.on_click(move |_| {
            let st = state_pm.borrow();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_pm.set_status_text("No article selected", 0);
                return;
            };
            if let Some(media_url) = article.media_url.clone().filter(|u| !u.is_empty()) {
                let chapters = st
                    .db
                    .as_ref()
                    .and_then(|db| db.chapters(&article.id).ok())
                    .unwrap_or_default();
                drop(st);
                player::open_player(&frame_pm, article, media_url, chapters);
                return;
            }
            if article.url.is_empty() {
                frame_pm.set_status_text("Article has no media or link", 0);
                return;
            }
            frame_pm.set_status_text("Resolving media with yt-dlp...", 0);
            let tx = tx_pm.clone();
            let id = article.id.clone();
            let url = article.url.clone();
            std::thread::spawn(move || {
                let result = blindrss_core::media::stream_url(&url).map(|stream| {
                    let info = blindrss_core::media::info(&url).ok();
                    (stream, info)
                });
                let _ = tx.send(RefreshMsg::MediaReady(id, result));
            });
        });

        // New Smart Folder: name + a simple title-contains rule
        // (blank matches everything). Full rule editing is a later
        // dialog; the rule engine already handles the full tree.
        let state_nsf = state.clone();
        let frame_nsf = frame;
        let tree_nsf = tree;
        let list_nsf = list;
        new_sf_btn.on_click(move |_| {
            let Some(name) = prompt_text(&frame_nsf, "Smart folder name:", "New Smart Folder", "")
            else {
                return;
            };
            if name.trim().is_empty() {
                return;
            }
            let needle = prompt_text(
                &frame_nsf,
                "Show articles whose title contains (blank = all):",
                "New Smart Folder",
                "",
            )
            .unwrap_or_default();
            let rule = if needle.trim().is_empty() {
                serde_json::json!({"match": "all", "conditions": []})
            } else {
                serde_json::json!({"match": "all", "conditions": [
                    {"field": "title", "op": "contains", "value": needle.trim()}
                ]})
            };
            let mut st = state_nsf.borrow_mut();
            if let Some(db) = &st.db {
                let _ = db.create_smart_folder(name.trim(), &rule.to_string());
            }
            rebuild_tree(&mut st, &tree_nsf);
            refill_articles(&mut st, &list_nsf);
            frame_nsf.set_status_text("Smart folder created", 0);
        });

        // Delete Smart Folder: deletes the smart folder selected in
        // the tree.
        let state_dsf = state.clone();
        let frame_dsf = frame;
        let tree_dsf = tree;
        let list_dsf = list;
        del_sf_btn.on_click(move |_| {
            let mut st = state_dsf.borrow_mut();
            let Some(id) = st
                .selected_feed
                .as_ref()
                .and_then(|f| f.strip_prefix("smart:"))
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
            else {
                frame_dsf.set_status_text("Select a smart folder in the tree first", 0);
                return;
            };
            if let Some(db) = &st.db {
                let _ = db.delete_smart_folder(&id);
            }
            st.selected_feed = None;
            rebuild_tree(&mut st, &tree_dsf);
            refill_articles(&mut st, &list_dsf);
            frame_dsf.set_status_text("Smart folder deleted", 0);
        });

        // Download: the selected article's media (direct) or page
        // (yt-dlp) into the configured download directory.
        let state_dl = state.clone();
        let frame_dl = frame;
        let tx_dl = tx.clone();
        download_btn.on_click(move |_| {
            let st = state_dl.borrow();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_dl.set_status_text("No article selected", 0);
                return;
            };
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let dir = cfg
                .raw()
                .get("download_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var("HOME")
                        .map(|h| PathBuf::from(h).join("Downloads").join("BlindRSS"))
                        .unwrap_or_else(|_| PathBuf::from("Downloads"))
                });
            frame_dl.set_status_text("Downloading...", 0);
            let tx = tx_dl.clone();
            std::thread::spawn(move || {
                let result =
                    if let Some(media) = article.media_url.clone().filter(|u| !u.is_empty()) {
                        blindrss_core::download::download_direct(&media, &dir, &article.title)
                            .map(|p| format!("Downloaded {}", p.display()))
                    } else if !article.url.is_empty() {
                        blindrss_core::download::download_with_ytdlp(
                            &article.url,
                            &dir,
                            blindrss_core::download::DownloadPreset::Best,
                        )
                        .map(|d| format!("Downloaded to {}", d.display()))
                    } else {
                        Err("Article has nothing to download".to_string())
                    };
                let _ = tx.send(RefreshMsg::DownloadDone(result));
            });
        });

        // Import YouTube Takeout: pick the ZIP, subscribe every
        // channel/playlist feed not already present.
        let state_to = state.clone();
        let frame_to = frame;
        let tree_to = tree;
        let list_to = list;
        takeout_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_to)
                .with_message("Select YouTube Takeout ZIP")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Open)
                .build();
            if dlg.show_modal() != 5100 {
                return;
            }
            let Some(path) = dlg.get_path() else { return };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    frame_to.set_status_text(&format!("Takeout read failed: {e}"), 0);
                    return;
                }
            };
            match blindrss_core::takeout::parse_takeout_zip(&bytes) {
                Ok(feeds) => {
                    let mut st = state_to.borrow_mut();
                    let new_feeds: Vec<Feed> = {
                        let existing: Vec<String> =
                            st.feeds.iter().map(|x| x.url.clone()).collect();
                        let mut acc = Vec::new();
                        if let Some(db) = &st.db {
                            for f in &feeds {
                                if existing.contains(&f.url) {
                                    continue;
                                }
                                if let Ok(feed) = db.add_feed(&f.url, &f.title, Some("YouTube")) {
                                    acc.push(feed);
                                }
                            }
                        }
                        acc
                    };
                    let added = new_feeds.len();
                    st.feeds.extend(new_feeds);
                    rebuild_tree(&mut st, &tree_to);
                    refill_articles(&mut st, &list_to);
                    frame_to
                        .set_status_text(&format!("Takeout: {added} new subscription(s) added"), 0);
                }
                Err(e) => frame_to.set_status_text(&format!("Takeout: {e}"), 0),
            }
        });

        // Rescan Archive: live document + newest Wayback snapshot
        // for the selected feed, on a worker thread/connection.
        let state_ar = state.clone();
        let frame_ar = frame;
        let tx_ar = tx.clone();
        archive_btn.on_click(move |_| {
            let feed = {
                let st = state_ar.borrow();
                st.selected_feed
                    .as_ref()
                    .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned())
            };
            let Some(feed) = feed else {
                frame_ar.set_status_text("Select a feed in the tree first", 0);
                return;
            };
            frame_ar.set_status_text("Rescanning archive...", 0);
            let tx = tx_ar.clone();
            std::thread::spawn(move || {
                let result = Db::open(&db_path())
                    .map_err(|e| format!("{e}"))
                    .and_then(|db| blindrss_core::archive::rescan_archive(&db, &feed));
                let _ = tx.send(RefreshMsg::ArchiveDone(feed.title.clone(), result));
            });
        });

        // Cast: resolve the article's stream, discover Chromecasts,
        // launch the default receiver and load the media — all on a
        // worker thread (SSDP + TLS would block the UI).
        let state_cast = state.clone();
        let frame_cast = frame;
        let tx_cast = tx.clone();
        cast_btn.on_click(move |_| {
            let article = {
                let st = state_cast.borrow();
                st.selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
            };
            let Some(article) = article else {
                frame_cast.set_status_text("No article selected", 0);
                return;
            };
            frame_cast.set_status_text("Looking for cast devices...", 0);
            let tx = tx_cast.clone();
            std::thread::spawn(move || {
                let result = (|| -> Result<String, String> {
                    let url = if let Some(m) = article.media_url.clone().filter(|u| !u.is_empty()) {
                        m
                    } else if !article.url.is_empty() {
                        blindrss_core::media::stream_url(&article.url)?
                    } else {
                        return Err("Article has no playable media".to_string());
                    };
                    let devices = blindrss_core::cast::discover_chromecasts(
                        std::time::Duration::from_secs(3),
                    );
                    let device = devices
                        .first()
                        .ok_or("No Chromecast found on this network")?;
                    let mut session = blindrss_core::castsession::CastSession::connect(
                        &device.host,
                        device.port,
                    )?;
                    session.launch_default_receiver()?;
                    let ctype = article
                        .media_type
                        .clone()
                        .filter(|t| t.contains('/'))
                        .unwrap_or_else(|| "audio/mpeg".to_string());
                    session.load_media(&url, &ctype, &article.title)?;
                    Ok(format!("Casting {} to {}", article.title, device.name))
                })();
                let _ = tx.send(RefreshMsg::CastDone(result));
            });
        });

        // Backup / Restore / Prune / Updates.
        let frame_bk = frame;
        backup_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_bk)
                .with_message("Save backup")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Save)
                .build();
            if dlg.show_modal() != 5100 {
                return;
            }
            if let Some(path) = dlg.get_path() {
                match blindrss_core::maintenance::create_backup(
                    &db_path(),
                    &config_path(),
                    &PathBuf::from(&path),
                ) {
                    Ok(()) => frame_bk.set_status_text(&format!("Backup saved to {path}"), 0),
                    Err(e) => frame_bk.set_status_text(&format!("Backup: {e}"), 0),
                }
            }
        });
        let state_rs = state.clone();
        let frame_rs = frame;
        let tree_rs = tree;
        let list_rs = list;
        let reader_rs = reader;
        restore_btn.on_click(move |_| {
            let dlg = FileDialog::builder(&frame_rs)
                .with_message("Restore backup")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Open)
                .build();
            if dlg.show_modal() != 5100 {
                return;
            }
            if let Some(path) = dlg.get_path() {
                match blindrss_core::maintenance::restore_backup(
                    &PathBuf::from(&path),
                    &db_path(),
                    &config_path(),
                ) {
                    Ok(()) => {
                        let mut st = state_rs.borrow_mut();
                        st.db = Db::open(&db_path()).ok();
                        st.feeds = st
                            .db
                            .as_ref()
                            .and_then(|db| db.feeds().ok())
                            .unwrap_or_default();
                        st.articles.clear();
                        rebuild_tree(&mut st, &tree_rs);
                        refill_articles(&mut st, &list_rs);
                        reader_rs.set_value("");
                        frame_rs.set_status_text("Backup restored", 0);
                    }
                    Err(e) => frame_rs.set_status_text(&format!("Restore: {e}"), 0),
                }
            }
        });
        let state_pr = state.clone();
        let frame_pr = frame;
        let list_pr = list;
        prune_btn.on_click(move |_| {
            let mut st = state_pr.borrow_mut();
            match st.db.as_ref().map(|db| db.prune_old_articles(30)) {
                Some(Ok(n)) => {
                    refill_articles(&mut st, &list_pr);
                    frame_pr.set_status_text(&format!("Pruned {n} old article(s)"), 0);
                }
                Some(Err(e)) => frame_pr.set_status_text(&format!("Prune: {e}"), 0),
                None => frame_pr.set_status_text("No database", 0),
            }
        });
        let frame_up = frame;
        let tx_up = tx.clone();
        update_btn.on_click(move |_| {
            frame_up.set_status_text("Checking for updates...", 0);
            let tx = tx_up.clone();
            std::thread::spawn(move || {
                let _ = tx.send(RefreshMsg::UpdateChecked(
                    blindrss_core::maintenance::check_update(),
                ));
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
                // Discovery chain: social transforms, direct feed,
                // then HTML alternate links.
                let result = blindrss_core::discovery::resolve_subscribe_url(&url);
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
                    Ok(RefreshMsg::Added(_url, Ok((feed_url, parsed)))) => {
                        let mut st = state_tick.borrow_mut();
                        if let Some(db) = st.db.as_ref() {
                            let title = if parsed.title.trim().is_empty() {
                                feed_url.clone()
                            } else {
                                parsed.title.clone()
                            };
                            match db.add_feed(&feed_url, &title, None) {
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
                    Ok(RefreshMsg::MediaReady(id, result)) => {
                        let st = state_tick.borrow();
                        let article = st.articles.iter().find(|a| a.id == id).cloned();
                        match (article, result) {
                            (Some(article), Ok((stream, info))) => {
                                let mut chapters = st
                                    .db
                                    .as_ref()
                                    .and_then(|db| db.chapters(&article.id).ok())
                                    .unwrap_or_default();
                                if let Some(info) = info
                                    && !info.chapters.is_empty()
                                {
                                    if let Some(db) = &st.db {
                                        let _ = db.replace_chapters(&article.id, &info.chapters);
                                    }
                                    chapters = info.chapters;
                                }
                                drop(st);
                                player::open_player(&frame_tick, article, stream, chapters);
                            }
                            (_, Err(e)) => frame_tick.set_status_text(&format!("Play: {e}"), 0),
                            _ => {}
                        }
                    }
                    Ok(RefreshMsg::DownloadDone(result)) => match result {
                        Ok(msg) => frame_tick.set_status_text(&msg, 0),
                        Err(e) => frame_tick.set_status_text(&format!("Download: {e}"), 0),
                    },
                    Ok(RefreshMsg::YoutubeText(id, result)) => {
                        let st = state_tick.borrow();
                        let title = st
                            .articles
                            .iter()
                            .find(|a| a.id == id)
                            .map(|a| a.title.clone())
                            .unwrap_or_default();
                        match result {
                            Ok(text) => {
                                reader.set_value(&format!("{title}\n\n{text}"));
                                frame_tick.set_status_text("Full text (YouTube)", 0);
                            }
                            Err(e) => frame_tick.set_status_text(&format!("Full text: {e}"), 0),
                        }
                    }
                    Ok(RefreshMsg::ArchiveDone(title, result)) => {
                        let mut st = state_tick.borrow_mut();
                        match result {
                            Ok(new) => {
                                st.feeds = st
                                    .db
                                    .as_ref()
                                    .and_then(|db| db.feeds().ok())
                                    .unwrap_or_default();
                                refill_articles(&mut st, &list);
                                frame_tick.set_status_text(
                                    &format!("{title}: archive rescan added {new}"),
                                    0,
                                );
                            }
                            Err(e) => frame_tick.set_status_text(&format!("{title}: {e}"), 0),
                        }
                    }
                    Ok(RefreshMsg::CastDone(result)) => match result {
                        Ok(msg) => frame_tick.set_status_text(&msg, 0),
                        Err(e) => frame_tick.set_status_text(&format!("Cast: {e}"), 0),
                    },
                    Ok(RefreshMsg::UpdateChecked(result)) => match result {
                        Ok((tag, _)) => {
                            if tag.trim_start_matches('v') == env!("CARGO_PKG_VERSION") {
                                frame_tick.set_status_text("BlindRSS is up to date", 0);
                            } else {
                                frame_tick.set_status_text(&format!("Update available: {tag}"), 0);
                            }
                        }
                        Err(e) => frame_tick.set_status_text(&format!("Updates: {e}"), 0),
                    },
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
