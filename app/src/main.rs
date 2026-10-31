//! BlindRSS (Rust) — main window over the existing Python database.
//! Slice 2: Refresh All (worker-thread fetch, main-thread store),
//! feed filtering via the tree, mark-read on open.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

mod help;
mod keys;
mod player;
mod settings;

use blindrss_core::feeds::fetch_and_parse;
use blindrss_core::{Article, Db, Feed, ParsedFeed, store_parsed};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use wxdragon::prelude::*;

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Per-user data directory, mirroring Python's `_user_data_dir`:
/// Windows %APPDATA%\BlindRSS, macOS ~/Library/Application
/// Support/BlindRSS, Linux $XDG_CONFIG_HOME/BlindRSS (or ~/.config).
fn user_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join("AppData").join("Roaming"))
            .join("BlindRSS")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("BlindRSS")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".config"))
            .join("BlindRSS")
    }
}

/// The folder the executable runs from (Python's APP_DIR).
fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve the data directory the way Python's ConfigManager does:
/// the user dir wins when config.json exists there, then the app
/// folder when config.json exists beside the executable, otherwise
/// the user dir (which may not exist yet).
fn data_dir() -> PathBuf {
    let user = user_data_dir();
    if user.join("config.json").exists() {
        return user;
    }
    let app = app_dir();
    if app.join("config.json").exists() {
        return app;
    }
    user
}

fn config_path() -> PathBuf {
    std::env::var("BLINDRSS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir().join("config.json"))
}

/// Append one line to blindrss.log in the data dir (startup and
/// panic diagnostics, the BlindFB startup.log pattern). Never fails
/// the caller.
fn log_line(msg: &str) {
    let path = data_dir().join("blindrss.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "[{secs}] {msg}");
    }
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
/// Speak/braille a configured announcement for an event
/// (PARITY 7.4). Config is re-read per event so Settings changes
/// apply live, as in Python.
fn announce(event: &str, message: &str) {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let _ = blindrss_core::announce::Announcements::from_config(&cfg).announce(event, message);
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

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

/// Put keyboard focus back on the surface the user was working in
/// after a modal dialog closes. Python returns focus to the tree
/// or the article list by context whenever a panel/dialog closes;
/// without this the screen reader lands nowhere useful.
fn restore_focus(state: &State, tree: &TreeCtrl, list: &ListCtrl) {
    if state.focus_list {
        list.set_focus();
    } else {
        tree.set_focus();
    }
}

/// prompt_text + focus restore to the working surface.
fn prompt_focus(
    parent: &impl WxWidget,
    message: &str,
    caption: &str,
    default: &str,
    state: &State,
    tree: &TreeCtrl,
    list: &ListCtrl,
) -> Option<String> {
    let value = prompt_text(parent, message, caption, default);
    restore_focus(state, tree, list);
    value
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BLINDRSS_DB") {
        return PathBuf::from(p);
    }
    // Python parity: rss.db lives in the resolved data dir. When it
    // is missing there but exists in the other location (user dir vs
    // app folder), copy it across so a data-location switch never
    // starts the user with an empty database.
    let dir = data_dir();
    let target = dir.join("rss.db");
    if !target.exists() {
        let user = user_data_dir();
        let alt = if dir == user {
            app_dir().join("rss.db")
        } else {
            user.join("rss.db")
        };
        if alt.exists() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::copy(&alt, &target);
        }
    }
    target
}

struct State {
    db: Option<Db>,
    feeds: Vec<Feed>,
    articles: Vec<Article>,
    selected_feed: Option<String>,
    selected_article: Option<usize>,
    refreshing: bool,
    /// Surface the user is working in: false = feed tree,
    /// true = article list. Modal dialogs restore focus to it.
    focus_list: bool,
    /// Config opt-in: persist/reopen the last selected feed.
    remember_last_feed: bool,
    /// Cooperative refresh cancellation, shared with the refresh
    /// worker thread (Python's cancel_refresh).
    cancel_refresh: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by the worker when a refresh ended early on cancel;
    /// the drain reads it on AllDone to announce the stop.
    refresh_stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When the last refresh finished (scheduled-refresh timer).
    last_refresh_end: Option<std::time::Instant>,
    /// Article view filters (Python's Ctrl+1..6): read filter
    /// 0 = all, 1 = unread only, 2 = read only; media filter
    /// 0 = all, 1 = with media, 2 = without media.
    read_filter: u8,
    media_filter: u8,
    /// Article sort (config article_sort_by / article_sort_ascending).
    sort_by: String,
    sort_ascending: bool,
    /// Last find-in-article query (Ctrl+F / F3 repeat it).
    find_query: String,
    /// True while the search text box has keyboard focus; the
    /// Backspace toggle-read shortcut must not eat its editing.
    search_focused: bool,
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

// Menu bar command ids (2100s; wx stock ids live below 6000 but the
// app dialog ids 5100+ are modal results, so keep menus clear of the
// button range and of each other).
const M_ADD_FEED: i32 = 2101;
const M_IMPORT_OPML: i32 = 2102;
const M_EXPORT_OPML: i32 = 2103;
const M_TAKEOUT: i32 = 2104;
const M_BACKUP: i32 = 2105;
const M_RESTORE: i32 = 2106;
const M_EXIT: i32 = 2107;
const M_REFRESH: i32 = 2111;
const M_REFRESH_FEED: i32 = 2120;
const M_SYNC: i32 = 2112;
const M_REMOVE_FEED: i32 = 2113;
const M_EDIT_FEED: i32 = 2114;
const M_RENAME_CAT: i32 = 2115;
const M_SET_CAT: i32 = 2116;
const M_NEW_SF: i32 = 2117;
const M_DEL_SF: i32 = 2118;
const M_ARCHIVE: i32 = 2119;
const M_TOGGLE_READ: i32 = 2121;
const M_FULLTEXT: i32 = 2122;
const M_FAVORITE: i32 = 2123;
const M_MARKALL: i32 = 2124;
const M_DELETE_ARTICLE: i32 = 2125;
const M_DELETED: i32 = 2126;
const M_PLAY: i32 = 2127;
const M_DOWNLOAD: i32 = 2128;
const M_CAST: i32 = 2129;
const M_SEARCH: i32 = 2130;
const M_SETTINGS: i32 = 2141;
const M_KEYS: i32 = 2142;
const M_PRUNE: i32 = 2143;
const M_UPDATE: i32 = 2144;
const M_HELP: i32 = 2151;
const M_COPY_URL: i32 = 2152;
const M_COPY_MEDIA: i32 = 2153;
const M_COPY_FEED_URL: i32 = 2154;
const M_REFRESH_CAT: i32 = 2155;
const M_STOP_REFRESH: i32 = 2156;
const M_ADD_CAT: i32 = 2157;
const M_DEL_CAT: i32 = 2158;
const M_IMPORT_HERE: i32 = 2159;
const M_EXPORT_CAT: i32 = 2160;
const M_FEED_ERRORS: i32 = 2161;
const M_FILTER_ALL: i32 = 2170;
const M_FILTER_UNREAD: i32 = 2171;
const M_FILTER_READ: i32 = 2172;
const M_MEDIA_ALL: i32 = 2173;
const M_MEDIA_WITH: i32 = 2174;
const M_MEDIA_WITHOUT: i32 = 2175;
const M_SORT_DATE: i32 = 2176;
const M_SORT_NAME: i32 = 2177;
const M_SORT_AUTHOR: i32 = 2178;
const M_SORT_FEED: i32 = 2179;
const M_SORT_DIR: i32 = 2180;
const M_FIND: i32 = 2181;
const M_FIND_NEXT: i32 = 2182;

/// Format a unix timestamp as "YYYY-MM-DD HH:MM" UTC (civil-date
/// conversion, no date crate needed at this precision).
fn fmt_ts(ts: Option<f64>) -> String {
    let Some(ts) = ts.filter(|t| *t > 0.0) else {
        return "never".to_string();
    };
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m) = (rem / 3600, (rem % 3600) / 60);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = if mo <= 2 { y + 1 } else { y };
    format!("{yr:04}-{mo:02}-{d:02} {h:02}:{m:02}")
}

/// The category path of the current tree selection, if any.
fn selected_category(st: &State) -> Option<String> {
    st.selected_feed
        .as_deref()
        .and_then(|f| f.strip_prefix("cat:"))
        .map(str::to_string)
}

/// Feeds inside a category path, subcategories included.
fn feeds_in_category(feeds: &[Feed], path: &str) -> Vec<Feed> {
    let prefix = format!("{path}/");
    feeds
        .iter()
        .filter(|f| {
            f.category.as_deref() == Some(path)
                || f.category
                    .as_deref()
                    .is_some_and(|c| c.starts_with(&prefix))
        })
        .cloned()
        .collect()
}

/// Build the live shortcut map: effective accelerator string ->
/// command id, from the registry plus the user's
/// `keyboard_shortcuts` overrides. Empty accelerators are skipped.
fn build_keymap() -> std::collections::HashMap<String, String> {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let mut overrides = std::collections::HashMap::new();
    if let Some(obj) = cfg
        .raw()
        .get("keyboard_shortcuts")
        .and_then(|v| v.as_object())
    {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                overrides.insert(k.clone(), s.to_string());
            }
        }
    }
    blindrss_core::shortcuts::effective(&overrides)
        .into_iter()
        .filter(|(_, accel)| !accel.is_empty())
        .map(|(id, accel)| (accel, id))
        .collect()
}

/// Case-insensitive substring search over chars, starting at
/// `from` (a char index) and wrapping once. Returns the match's
/// (start, end) char positions.
fn find_in_text(text: &str, query: &str, from: usize) -> Option<(i64, i64)> {
    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let eq = |a: char, b: char| {
        a.to_lowercase().next().unwrap_or(a) == b.to_lowercase().next().unwrap_or(b)
    };
    let matches_at = |start: usize| -> bool {
        needle
            .iter()
            .enumerate()
            .all(|(i, n)| eq(hay[start + i], *n))
    };
    let last_start = hay.len() - needle.len();
    let from = from.min(last_start);
    for start in from..=last_start {
        if matches_at(start) {
            return Some((start as i64, (start + needle.len()) as i64));
        }
    }
    for start in 0..from {
        if matches_at(start) {
            return Some((start as i64, (start + needle.len()) as i64));
        }
    }
    None
}

fn feed_label(db: &Db, feed: &Feed) -> String {
    match db.unread_count(Some(&feed.id)) {
        Ok(n) if n > 0 => format!("{} ({n})", feed.title),
        _ => feed.title.clone(),
    }
}

/// Find a tree item by its filter data, depth-first from `item`.
fn find_item_by_data(tree: &TreeCtrl, item: &TreeItemId, data: &str) -> Option<TreeItemId> {
    let mine = tree
        .get_custom_data_direct(item)
        .and_then(|d| d.downcast_ref::<String>().cloned());
    if mine.as_deref() == Some(data) {
        return Some(item.clone());
    }
    let (mut child, mut cookie) = tree.get_first_child(item)?;
    loop {
        if let Some(found) = find_item_by_data(tree, &child, data) {
            return Some(found);
        }
        child = tree.get_next_child(item, &mut cookie)?;
    }
}

fn rebuild_tree(state: &mut State, tree: &TreeCtrl) {
    // Python's tree rebuild always ends with a selection (the saved
    // feed, else All Feeds): a focused tree with no selected item
    // gives NVDA nothing to announce. Preserve the current selection
    // by its filter data; fall back to state, then to the root.
    let previous = tree
        .get_selection()
        .and_then(|sel| {
            tree.get_custom_data_direct(&sel)
                .and_then(|d| d.downcast_ref::<String>().cloned())
        })
        .or_else(|| state.selected_feed.clone())
        .unwrap_or_default();
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
    // Category grouping (Python parity): a node per category path
    // ("News", "News/Tech"), feeds under their category node,
    // uncategorized feeds at the root. Node data is "cat:<path>".
    let expand_cats = blindrss_core::config::Config::load(&config_path())
        .unwrap_or_default()
        .get_bool("category_tree_default_expanded", true);
    let mut paths: Vec<String> = db
        .categories()
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.title)
        .collect();
    for f in &feeds {
        if let Some(cat) = &f.category
            && !cat.trim().is_empty()
            && !paths.contains(cat)
        {
            paths.push(cat.clone());
        }
    }
    paths.sort();
    let mut cat_items: std::collections::HashMap<String, TreeItemId> =
        std::collections::HashMap::new();
    for path in &paths {
        let mut cur = String::new();
        for seg in path.split('/').filter(|s| !s.trim().is_empty()) {
            let parent = if cur.is_empty() {
                root.clone()
            } else {
                cat_items.get(&cur).cloned().unwrap_or_else(|| root.clone())
            };
            if !cur.is_empty() {
                cur.push('/');
            }
            cur.push_str(seg.trim());
            if !cat_items.contains_key(&cur) {
                // Node label: leaf name plus the unread total of
                // every feed in scope, Python-style.
                let prefix = format!("{cur}/");
                let unread: i64 = feeds
                    .iter()
                    .filter(|f| {
                        f.category.as_deref() == Some(cur.as_str())
                            || f.category
                                .as_deref()
                                .is_some_and(|c| c.starts_with(&prefix))
                    })
                    .map(|f| db.unread_count(Some(&f.id)).unwrap_or(0))
                    .sum();
                let label = if unread > 0 {
                    format!("{} ({unread})", seg.trim())
                } else {
                    seg.trim().to_string()
                };
                if let Some(item) =
                    tree.append_item_with_data(&parent, &label, format!("cat:{cur}"), None, None)
                {
                    cat_items.insert(cur.clone(), item);
                }
            }
        }
    }
    for f in &feeds {
        let label = feed_label(db, f);
        let parent = f
            .category
            .as_ref()
            .filter(|c| !c.trim().is_empty())
            .and_then(|c| cat_items.get(c))
            .cloned()
            .unwrap_or_else(|| root.clone());
        tree.append_item_with_data(&parent, &label, f.id.clone(), None, None);
    }
    if expand_cats {
        for item in cat_items.values() {
            tree.expand(item);
        }
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
    // Restore (or default) the selection, mirroring Python's
    // SelectItem(selection_target) at the end of every tree build.
    let found = find_item_by_data(tree, &root, &previous);
    let keep = found.is_some() && !previous.is_empty();
    let target = found.unwrap_or(root);
    tree.select_item(&target);
    tree.set_focused_item(&target);
    tree.ensure_visible(&target);
    state.selected_feed = if keep { Some(previous) } else { None };
}

fn refill_articles(state: &mut State, list: &ListCtrl) {
    list.clear_all();
    list.insert_column(0, "Title", ListColumnFormat::Left, 420);
    list.insert_column(1, "Date", ListColumnFormat::Left, 160);
    list.insert_column(2, "Feed", ListColumnFormat::Left, 160);
    state.articles.clear();
    let Some(db) = state.db.as_ref() else { return };
    let filter = state.selected_feed.as_deref();
    let mut loaded: Vec<Article> = Vec::new();
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
                loaded = matched;
            }
        }
        return finish_refill(state, list, loaded);
    }
    // Category selection: articles across every feed in the
    // category, subcategories included.
    if let Some(path) = filter.and_then(|f| f.strip_prefix("cat:")) {
        let ids: Vec<String> = feeds_in_category(&state.feeds, path)
            .iter()
            .map(|f| f.id.clone())
            .collect();
        let loaded = db.articles_for_feeds(&ids, 500).unwrap_or_default();
        return finish_refill(state, list, loaded);
    }
    let loaded = db.articles(filter, 300).unwrap_or_default();
    finish_refill(state, list, loaded);
}

/// Apply the view filters (read + media) and the sort, then render.
/// Python applies the same two filter groups (Ctrl+1..6) over
/// whatever the current view loaded.
fn finish_refill(state: &mut State, list: &ListCtrl, mut loaded: Vec<Article>) {
    match state.read_filter {
        1 => loaded.retain(|a| !a.is_read),
        2 => loaded.retain(|a| a.is_read),
        _ => {}
    }
    match state.media_filter {
        1 => loaded.retain(|a| a.media_url.as_deref().is_some_and(|m| !m.is_empty())),
        2 => loaded.retain(|a| a.media_url.as_deref().is_none_or(|m| m.is_empty())),
        _ => {}
    }
    let feed_titles: std::collections::HashMap<&str, &str> = state
        .feeds
        .iter()
        .map(|f| (f.id.as_str(), f.title.as_str()))
        .collect();
    match state.sort_by.as_str() {
        "name" => loaded.sort_by_key(|a| a.title.to_lowercase()),
        "author" => loaded.sort_by_key(|a| a.author.as_deref().unwrap_or("").to_lowercase()),
        "feed" => loaded.sort_by_key(|a| {
            feed_titles
                .get(a.feed_id.as_str())
                .copied()
                .unwrap_or("")
                .to_lowercase()
        }),
        _ => {} // "date": the queries already return newest first.
    }
    // Non-date sorts are built ascending and flipped when the
    // direction is descending; the stored date order is already
    // descending, so ascending flips it instead.
    if state.sort_by == "date" {
        if state.sort_ascending {
            loaded.reverse();
        }
    } else if !state.sort_ascending {
        loaded.reverse();
    }
    for (i, a) in loaded.iter().enumerate() {
        list.insert_item(i as i64, &a.title, None);
        list.set_item_text_by_column(i as i64, 1, a.date.as_deref().unwrap_or(""));
        list.set_item_text_by_column(i as i64, 2, &a.feed_id);
    }
    state.articles = loaded;
}

/// UI language: config `language` ("auto"/missing = OS locale),
/// normalised to a primary tag ("de_DE.UTF-8" -> "de").
fn ui_language() -> String {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let chosen = cfg.get_str("language", "auto").to_string();
    let raw = if chosen.is_empty() || chosen == "auto" {
        std::env::var("LANG").unwrap_or_else(|_| "en".to_string())
    } else {
        chosen
    };
    let primary = raw
        .split(['_', '-', '.'])
        .next()
        .unwrap_or("en")
        .to_lowercase();
    if primary.is_empty() {
        "en".to_string()
    } else {
        primary
    }
}

fn locale_root() -> PathBuf {
    if let Ok(p) = std::env::var("BLINDRSS_LOCALE") {
        return PathBuf::from(p);
    }
    // Release layout: locale/ ships beside the executable.
    let beside_exe = app_dir().join("locale");
    if beside_exe.exists() {
        return beside_exe;
    }
    // Dev fallback: the Python checkout's locale tree.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../blindrss-work/locale")
}

fn main() {
    // Panics land in blindrss.log in the data dir instead of dying
    // silently (a screen reader user gets no console to read).
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        log_line(&format!("PANIC {msg} at {loc}"));
    }));
    log_line(&format!(
        "BlindRSS {} starting; db={} config={}",
        env!("CARGO_PKG_VERSION"),
        db_path().display(),
        config_path().display()
    ));
    let _ = wxdragon::main(|_| {
        let language = ui_language();
        let catalog = blindrss_core::i18n::Catalog::load(&locale_root(), &language);
        let refresh_label = catalog.get("Refresh All Feeds").to_string();
        let frame = Frame::builder()
            .with_title("BlindRSS")
            .with_size(Size::new(1100, 700))
            .build();
        frame.create_status_bar(1, 0, -1, "status");

        // Tray icon (orange square placeholder artwork; the real
        // app icon ships with the release packaging).
        {
            let tray = TaskBarIcon::builder().build();
            let mut rgba = Vec::with_capacity(32 * 32 * 4);
            for _ in 0..32 * 32 {
                rgba.extend_from_slice(&[240, 130, 20, 255]);
            }
            if let Some(bmp) = Bitmap::from_rgba(&rgba, 32, 32) {
                tray.set_icon(&bmp, "BlindRSS");
            }
            Box::leak(Box::new(tray));
        }

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
        let cast_btn = Button::builder(&frame).with_label("Cast").build();
        let settings_btn = Button::builder(&frame).with_label("Settings...").build();
        let refresh_btn = Button::builder(&frame).with_label(&refresh_label).build();
        let help_btn = Button::builder(&frame).with_label("Help").build();
        let keys_btn = Button::builder(&frame).with_label("Shortcuts...").build();

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
        btn_row.add(&fulltext_btn, 0, SizerFlag::All, 4);
        btn_row.add(&toggle_read_btn, 0, SizerFlag::All, 4);
        btn_row.add(&edit_feed_btn, 0, SizerFlag::All, 4);
        btn_row.add(&rename_cat_btn, 0, SizerFlag::All, 4);
        btn_row.add(&set_cat_btn, 0, SizerFlag::All, 4);
        btn_row.add(&play_btn, 0, SizerFlag::All, 4);
        btn_row.add(&new_sf_btn, 0, SizerFlag::All, 4);
        btn_row.add(&del_sf_btn, 0, SizerFlag::All, 4);
        btn_row.add(&download_btn, 0, SizerFlag::All, 4);
        btn_row.add(&cast_btn, 0, SizerFlag::All, 4);
        btn_row.add(&settings_btn, 0, SizerFlag::All, 4);
        btn_row.add(&help_btn, 0, SizerFlag::All, 4);
        btn_row.add(&keys_btn, 0, SizerFlag::All, 4);
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
            focus_list: false,
            remember_last_feed: false,
            cancel_refresh: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            refresh_stopped: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_refresh_end: None,
            read_filter: 0,
            media_filter: 0,
            sort_by: "date".to_string(),
            sort_ascending: false,
            find_query: String::new(),
            search_focused: false,
        }));

        // A staged restore (Python backup.py semantics) applies
        // before the database opens; the replaced files are kept
        // as .pre-restore siblings.
        let _ = blindrss_core::maintenance::apply_pending_restore(&data_dir());
        // Open the database and populate (creating it on a fresh
        // install, as Python does).
        let path = db_path();
        {
            let mut st = state.borrow_mut();
            match Db::open_or_create(&path) {
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
        // Startup state restore (Python parity): reopen on the
        // feed that was selected when the app last closed, when
        // the user opted in with remember_last_feed.
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let mut st = state.borrow_mut();
            st.remember_last_feed = cfg.get_bool("remember_last_feed", false);
            st.sort_by = cfg.get_str("article_sort_by", "date").to_string();
            st.sort_ascending = cfg.get_bool("article_sort_ascending", false);
            if st.remember_last_feed {
                let last = cfg.get_str("last_selected_feed", "");
                if !last.is_empty() {
                    st.selected_feed = Some(last.to_string());
                }
            }
        }
        {
            let mut st = state.borrow_mut();
            rebuild_tree(&mut st, &tree);
            refill_articles(&mut st, &list);
        }

        // Track search-box focus so the list's Backspace
        // shortcut never eats text editing there.
        {
            let state_sf = state.clone();
            search_box.on_set_focus(move |_| {
                state_sf.borrow_mut().search_focused = true;
            });
            let state_kf = state.clone();
            search_box.on_kill_focus(move |_| {
                state_kf.borrow_mut().search_focused = false;
            });
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
            st.focus_list = false;
            if st.remember_last_feed {
                let value = st.selected_feed.clone().unwrap_or_default();
                let path = config_path();
                let mut cfg = blindrss_core::config::Config::load(&path).unwrap_or_default();
                if cfg.get_str("last_selected_feed", "") != value {
                    cfg.set_path("last_selected_feed", serde_json::json!(value));
                    let _ = cfg.save(&path);
                }
            }
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
            st.focus_list = true;
            let Some(a) = st.articles.get(idx as usize).cloned() else {
                return;
            };
            let body = a
                .content
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| a.description.clone())
                .unwrap_or_else(|| "(no stored text)".to_string());
            // Stored bodies are HTML; the reader shows clean text.
            let body = blindrss_core::extract::html_to_text(&body);
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
        fn spawn_refresh(
            feeds: Vec<Feed>,
            tx: Sender<RefreshMsg>,
            cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
            stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ) {
            std::thread::spawn(move || {
                for feed in feeds {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    let result = fetch_and_parse(&feed.url);
                    let _ = tx.send(RefreshMsg::FeedDone(feed, result));
                }
                let _ = tx.send(RefreshMsg::AllDone);
            });
        }
        let (tx, rx): (Sender<RefreshMsg>, Receiver<RefreshMsg>) = channel();
        let rx = Rc::new(RefCell::new(rx));
        let state_btn = state.clone();
        let frame_btn = frame;
        let tx_refresh = tx.clone();
        let act_refresh: Rc<dyn Fn()> = Rc::new(move || {
            let (feeds, cancel, stopped) = {
                let mut st = state_btn.borrow_mut();
                if st.refreshing {
                    return;
                }
                st.refreshing = true;
                st.cancel_refresh
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                st.refresh_stopped
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                (
                    st.feeds.clone(),
                    st.cancel_refresh.clone(),
                    st.refresh_stopped.clone(),
                )
            };
            frame_btn.set_status_text(&format!("Refreshing {} feeds...", feeds.len()), 0);
            announce("start_update", "Refresh Feeds");
            spawn_refresh(feeds, tx_refresh.clone(), cancel, stopped);
        });
        {
            let a = act_refresh.clone();
            refresh_btn.on_click(move |_| a());
        }

        // Refresh Feed (menu/context only): fetch just the feed
        // selected in the tree, reusing the same worker + channel +
        // AllDone completion path as Refresh All.
        let act_refresh_feed: Rc<dyn Fn()> = {
            let state_rf = state.clone();
            let frame_rf = frame;
            let tx_rf = tx.clone();
            Rc::new(move || {
                let feed = {
                    let mut st = state_rf.borrow_mut();
                    if st.refreshing {
                        return;
                    }
                    let found = st
                        .selected_feed
                        .as_ref()
                        .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned());
                    if found.is_some() {
                        st.refreshing = true;
                    }
                    found
                };
                let Some(feed) = feed else {
                    frame_rf.set_status_text("Select a feed in the tree first", 0);
                    return;
                };
                frame_rf.set_status_text(&format!("Refreshing {}...", feed.title), 0);
                announce("start_update", "Refresh Feeds");
                let (cancel, stopped) = {
                    let st = state_rf.borrow();
                    st.cancel_refresh
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    st.refresh_stopped
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    (st.cancel_refresh.clone(), st.refresh_stopped.clone())
                };
                spawn_refresh(vec![feed], tx_rf.clone(), cancel, stopped);
            })
        };

        // Article actions: delete (tombstoned), favorite, mark all
        // read, restore-deleted dialog, and search.
        let state_del = state.clone();
        let frame_del = frame;
        let act_delete: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_delete.clone();
            delete_btn.on_click(move |_| a());
        }

        let state_fav = state.clone();
        let frame_fav = frame;
        let act_fav: Rc<dyn Fn()> = Rc::new(move || {
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
            announce(
                "favorite_toggle",
                if new_value {
                    "Added to favorites"
                } else {
                    "Removed from favorites"
                },
            );
        });
        {
            let a = act_fav.clone();
            fav_btn.on_click(move |_| a());
        }

        let state_ma = state.clone();
        let frame_ma = frame;
        let act_markall: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_markall.clone();
            markall_btn.on_click(move |_| a());
        }

        let state_s = state.clone();
        let frame_s = frame;
        let act_search: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_search.clone();
            search_btn.on_click(move |_| a());
        }

        let state_deleted = state.clone();
        let frame_deleted = frame;
        let act_deleted: Rc<dyn Fn()> = Rc::new(move || {
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
            restore_focus(&state_deleted.borrow(), &tree, &list);
        });
        {
            let a = act_deleted.clone();
            deleted_btn.on_click(move |_| a());
        }

        // Sync Providers: for each hosted provider configured in
        // config.json, run its sync on a worker thread with its own
        // database connection (SQLite allows concurrent connections;
        // the main thread reloads when the result lands).
        let frame_sync = frame;
        let tx_sync = tx.clone();
        let act_sync: Rc<dyn Fn()> = Rc::new(move || {
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
        let act_fulltext: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_fulltext.clone();
            fulltext_btn.on_click(move |_| a());
        }

        // Toggle Read: flips the selected article locally and
        // propagates to the hosted provider when it came from one.
        let state_tr = state.clone();
        let frame_tr = frame;
        let list_tr = list;
        let act_toggle_read: Rc<dyn Fn()> = Rc::new(move || {
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
            announce("status_toggle", if new_state { "Read" } else { "Unread" });
            drop(st);
            propagate_read_state(&article, new_state);
        });
        {
            let a = act_toggle_read.clone();
            toggle_read_btn.on_click(move |_| a());
        }

        // Edit Feed: title + URL prompts for the tree-selected feed.
        let state_ef = state.clone();
        let frame_ef = frame;
        let tree_ef = tree;
        let list_ef = list;
        let act_edit_feed: Rc<dyn Fn()> = Rc::new(move || {
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
            let Some(title) = prompt_focus(
                &frame_ef,
                "Feed title:",
                "Edit Feed",
                &feed.title,
                &state_ef.borrow(),
                &tree_ef,
                &list_ef,
            ) else {
                return;
            };
            let Some(url) = prompt_focus(
                &frame_ef,
                "Feed URL:",
                "Edit Feed",
                &feed.url,
                &state_ef.borrow(),
                &tree_ef,
                &list_ef,
            ) else {
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
        {
            let a = act_edit_feed.clone();
            edit_feed_btn.on_click(move |_| a());
        }

        // Rename Category: renames the selected feed's category
        // everywhere (category row + all feeds in it).
        let state_rc = state.clone();
        let frame_rc = frame;
        let tree_rc = tree;
        let list_rc = list;
        let act_rename_cat: Rc<dyn Fn()> = Rc::new(move || {
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
            let Some(new_name) = prompt_focus(
                &frame_rc,
                "New category name:",
                "Rename Category",
                &category,
                &state_rc.borrow(),
                &tree_rc,
                &list_rc,
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
        {
            let a = act_rename_cat.clone();
            rename_cat_btn.on_click(move |_| a());
        }

        // Set Feed Category: prompt (blank = uncategorized); a new
        // name creates the category.
        let state_sc = state.clone();
        let frame_sc = frame;
        let tree_sc = tree;
        let list_sc = list;
        let act_set_cat: Rc<dyn Fn()> = Rc::new(move || {
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
            let Some(name) = prompt_focus(
                &frame_sc,
                "Category (blank for none):",
                "Set Feed Category",
                feed.category.as_deref().unwrap_or(""),
                &state_sc.borrow(),
                &tree_sc,
                &list_sc,
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
        {
            let a = act_set_cat.clone();
            set_cat_btn.on_click(move |_| a());
        }

        // Play Media: direct media URL plays immediately; a page
        // URL (YouTube etc.) resolves through yt-dlp on a worker
        // thread first.
        let state_pm = state.clone();
        let frame_pm = frame;
        let tx_pm = tx.clone();
        let act_play: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_pm.borrow();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_pm.set_status_text("No article selected", 0);
                return;
            };
            // Direct media (a playable enclosure) plays at once.
            // Anything else goes through the resolution chain on a
            // worker: Python never feeds an arbitrary article page
            // straight to yt-dlp (podcast pages without enclosures
            // failed with a raw "Unsupported URL" error).
            if let Some(media_url) =
                article
                    .media_url
                    .clone()
                    .filter(|u| !u.is_empty())
                    .filter(|u| {
                        blindrss_core::media::is_playable_media(u, article.media_type.as_deref())
                    })
            {
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
            frame_pm.set_status_text("Finding playable media...", 0);
            let tx = tx_pm.clone();
            let id = article.id.clone();
            std::thread::spawn(move || {
                let result = blindrss_core::media::resolve_for_play(
                    article.media_url.as_deref(),
                    article.media_type.as_deref(),
                    &article.url,
                    article.content.as_deref(),
                )
                .map(|r| {
                    if r.detected
                        && let Ok(db) = Db::open(&db_path())
                    {
                        let _ =
                            db.update_article_media(&article.id, &r.url, r.media_type.as_deref());
                    }
                    let info = if r.via_ytdlp {
                        blindrss_core::media::info(&article.url).ok()
                    } else {
                        None
                    };
                    (r.url, info)
                });
                let _ = tx.send(RefreshMsg::MediaReady(id, result));
            });
        });
        {
            let a = act_play.clone();
            play_btn.on_click(move |_| a());
        }

        // New Smart Folder: name + a simple title-contains rule
        // (blank matches everything). Full rule editing is a later
        // dialog; the rule engine already handles the full tree.
        let state_nsf = state.clone();
        let frame_nsf = frame;
        let tree_nsf = tree;
        let list_nsf = list;
        let act_new_sf: Rc<dyn Fn()> = Rc::new(move || {
            let Some(name) = prompt_focus(
                &frame_nsf,
                "Smart folder name:",
                "New Smart Folder",
                "",
                &state_nsf.borrow(),
                &tree_nsf,
                &list_nsf,
            ) else {
                return;
            };
            if name.trim().is_empty() {
                return;
            }
            let needle = prompt_focus(
                &frame_nsf,
                "Show articles whose title contains (blank = all):",
                "New Smart Folder",
                "",
                &state_nsf.borrow(),
                &tree_nsf,
                &list_nsf,
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
        {
            let a = act_new_sf.clone();
            new_sf_btn.on_click(move |_| a());
        }

        // Delete Smart Folder: deletes the smart folder selected in
        // the tree.
        let state_dsf = state.clone();
        let frame_dsf = frame;
        let tree_dsf = tree;
        let list_dsf = list;
        let act_del_sf: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_del_sf.clone();
            del_sf_btn.on_click(move |_| a());
        }

        // Download: the selected article's media (direct) or page
        // (yt-dlp) into the configured download directory.
        let state_dl = state.clone();
        let frame_dl = frame;
        let tx_dl = tx.clone();
        let act_download: Rc<dyn Fn()> = Rc::new(move || {
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
                let result = if let Some(media) = article
                    .media_url
                    .clone()
                    .filter(|u| !u.is_empty())
                    .filter(|u| {
                        blindrss_core::media::is_playable_media(u, article.media_type.as_deref())
                    }) {
                    blindrss_core::download::download_direct(&media, &dir, &article.title)
                        .map(|p| format!("Downloaded {}", p.display()))
                } else if !article.url.is_empty() {
                    // Same resolution as Play: yt-dlp only for
                    // supported sites or as the last resort; other
                    // pages are scanned for media first.
                    match blindrss_core::media::resolve_for_play(
                        article.media_url.as_deref(),
                        article.media_type.as_deref(),
                        &article.url,
                        article.content.as_deref(),
                    ) {
                        Ok(r) if r.via_ytdlp => blindrss_core::download::download_with_ytdlp(
                            &article.url,
                            &dir,
                            blindrss_core::download::DownloadPreset::Best,
                        )
                        .map(|d| format!("Downloaded to {}", d.display())),
                        Ok(r) => {
                            blindrss_core::download::download_direct(&r.url, &dir, &article.title)
                                .map(|p| format!("Downloaded {}", p.display()))
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Err("Article has nothing to download".to_string())
                };
                let _ = tx.send(RefreshMsg::DownloadDone(result));
            });
        });
        {
            let a = act_download.clone();
            download_btn.on_click(move |_| a());
        }

        // Import YouTube Takeout: pick the ZIP, subscribe every
        // channel/playlist feed not already present.
        let state_to = state.clone();
        let frame_to = frame;
        let tree_to = tree;
        let list_to = list;
        let act_takeout: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = FileDialog::builder(&frame_to)
                .with_message("Select YouTube Takeout ZIP")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Open)
                .build();
            let rc = dlg.show_modal();
            restore_focus(&state_to.borrow(), &tree_to, &list_to);
            if rc != 5100 {
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
        let act_archive: Rc<dyn Fn()> = Rc::new(move || {
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
        let act_cast: Rc<dyn Fn()> = Rc::new(move || {
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
        {
            let a = act_cast.clone();
            cast_btn.on_click(move |_| a());
        }

        // Backup / Restore / Prune / Updates.
        let frame_bk = frame;
        let state_bk = state.clone();
        let tree_bk = tree;
        let list_bk = list;
        let act_backup: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = FileDialog::builder(&frame_bk)
                .with_message("Save backup")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Save)
                .build();
            let rc = dlg.show_modal();
            restore_focus(&state_bk.borrow(), &tree_bk, &list_bk);
            if rc != 5100 {
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
        let act_restore: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = FileDialog::builder(&frame_rs)
                .with_message("Restore backup")
                .with_wildcard("ZIP files (*.zip)|*.zip")
                .with_style(FileDialogStyle::Open)
                .build();
            let rc = dlg.show_modal();
            restore_focus(&state_rs.borrow(), &tree_rs, &list_rs);
            if rc != 5100 {
                return;
            }
            if let Some(path) = dlg.get_path() {
                // Python stages a restore and applies it at the
                // next start, before the database opens — swapping
                // rss.db live under the open connection is unsafe.
                match blindrss_core::maintenance::stage_restore(&PathBuf::from(&path), &data_dir())
                {
                    Ok(()) => {
                        frame_rs.set_status_text("Restore staged — restart BlindRSS to apply it", 0)
                    }
                    Err(e) => frame_rs.set_status_text(&format!("Restore: {e}"), 0),
                }
            }
        });
        let state_pr = state.clone();
        let frame_pr = frame;
        let list_pr = list;
        let act_prune: Rc<dyn Fn()> = Rc::new(move || {
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
        let act_update: Rc<dyn Fn()> = Rc::new(move || {
            frame_up.set_status_text("Checking for updates...", 0);
            let tx = tx_up.clone();
            std::thread::spawn(move || {
                let _ = tx.send(RefreshMsg::UpdateChecked(
                    blindrss_core::maintenance::check_update(),
                ));
            });
        });

        // Settings: provider accounts + download folder + Inoreader
        // OAuth connect, in a modal dialog. Maintenance actions live
        // in the dialog now (Brandon, 2026-10-30: the main page was
        // crowded with them); the dialog reports which one was
        // chosen and it runs here, after the dialog closes.
        let frame_set = frame;
        let state_set = state.clone();
        let tree_set = tree;
        let list_set = list;
        type Action = Rc<dyn Fn()>;
        let dispatch: Vec<(&'static str, Action)> = vec![
            ("backup", act_backup.clone()),
            ("restore", act_restore.clone()),
            ("takeout", act_takeout.clone()),
            ("archive", act_archive.clone()),
            ("sync", act_sync.clone()),
            ("prune", act_prune.clone()),
            ("update", act_update.clone()),
        ];
        let act_settings: Rc<dyn Fn()> = Rc::new(move || {
            let intent = settings::show_settings(&frame_set, &config_path());
            restore_focus(&state_set.borrow(), &tree_set, &list_set);
            if let Some(intent) = intent
                && let Some((_, action)) = dispatch.iter().find(|(name, _)| *name == intent)
            {
                action();
            }
        });
        {
            let a = act_settings.clone();
            settings_btn.on_click(move |_| a());
        }

        // The live shortcut map for the frame char hook; the
        // shortcuts dialog refreshes it when it closes.
        let keymap: Rc<RefCell<std::collections::HashMap<String, String>>> =
            Rc::new(RefCell::new(build_keymap()));

        // Help viewer + shortcuts registry dialogs.
        let frame_help = frame;
        let state_help = state.clone();
        let tree_help = tree;
        let list_help = list;
        let language_help = language.clone();
        let act_help: Rc<dyn Fn()> = Rc::new(move || {
            help::show_help(&frame_help, &language_help);
            restore_focus(&state_help.borrow(), &tree_help, &list_help);
        });
        {
            let a = act_help.clone();
            help_btn.on_click(move |_| a());
        }
        let frame_keys = frame;
        let state_keys = state.clone();
        let tree_keys = tree;
        let list_keys = list;
        let keymap_keys = keymap.clone();
        let act_keys: Rc<dyn Fn()> = Rc::new(move || {
            keys::show_shortcuts(&frame_keys, &config_path());
            *keymap_keys.borrow_mut() = build_keymap();
            restore_focus(&state_keys.borrow(), &tree_keys, &list_keys);
        });
        {
            let a = act_keys.clone();
            keys_btn.on_click(move |_| a());
        }

        // Add Feed: prompt for a URL, fetch it on a worker thread to
        // learn its title, then subscribe + store its current articles.
        let state_add = state.clone();
        let frame_add = frame;
        let tree_add = tree;
        let list_add = list;
        let tx_add = tx.clone();
        let act_add: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = TextEntryDialog::builder(&frame_add, "Feed URL:", "Add Feed").build();
            let rc = dlg.show_modal();
            restore_focus(&state_add.borrow(), &tree_add, &list_add);
            if rc != ID_OK {
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
        {
            let a = act_add.clone();
            add_btn.on_click(move |_| a());
        }

        // Remove Feed: removes the feed selected in the tree, after a
        // confirmation (it also deletes that feed's stored articles).
        let state_rm = state.clone();
        let frame_rm = frame;
        let act_remove: Rc<dyn Fn()> = Rc::new(move || {
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
            let rc = dlg.show_modal();
            restore_focus(&state_rm.borrow(), &tree, &list);
            if rc != ID_YES {
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
        {
            let a = act_remove.clone();
            remove_btn.on_click(move |_| a());
        }

        // OPML import/export.
        let state_imp = state.clone();
        let frame_imp = frame;
        let act_import: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = FileDialog::builder(&frame_imp)
                .with_message("Import OPML")
                .with_wildcard("OPML files (*.opml;*.xml)|*.opml;*.xml")
                .with_style(FileDialogStyle::Open)
                .build();
            let rc = dlg.show_modal();
            restore_focus(&state_imp.borrow(), &tree, &list);
            if rc != ID_OK {
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
        {
            let a = act_import.clone();
            import_btn.on_click(move |_| a());
        }

        let state_exp = state.clone();
        let frame_exp = frame;
        let act_export: Rc<dyn Fn()> = Rc::new(move || {
            let dlg = FileDialog::builder(&frame_exp)
                .with_message("Export OPML")
                .with_wildcard("OPML files (*.opml)|*.opml")
                .with_style(FileDialogStyle::Save)
                .build();
            let rc = dlg.show_modal();
            restore_focus(&state_exp.borrow(), &tree, &list);
            if rc != ID_OK {
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
        {
            let a = act_export.clone();
            export_btn.on_click(move |_| a());
        }

        // Menu bar (PARITY Audit 2, 5C/5F.1): every item invokes the
        // same Rc action as its button, so menus and buttons can
        // never drift apart. Mnemonics only for now; accelerators
        // arrive with the live shortcut dispatch.
        let state_cu = state.clone();
        let frame_cu = frame;
        let act_copy_url: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_cu.borrow();
            let Some(article) = st.selected_article.and_then(|i| st.articles.get(i)) else {
                frame_cu.set_status_text("No article selected", 0);
                return;
            };
            if article.url.is_empty() {
                frame_cu.set_status_text("Article has no URL", 0);
                return;
            }
            let ok = Clipboard::get().set_text(&article.url);
            frame_cu.set_status_text(
                if ok {
                    "Article URL copied"
                } else {
                    "Copy failed"
                },
                0,
            );
        });
        let state_cm = state.clone();
        let frame_cm = frame;
        let act_copy_media: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_cm.borrow();
            let Some(article) = st.selected_article.and_then(|i| st.articles.get(i)) else {
                frame_cm.set_status_text("No article selected", 0);
                return;
            };
            let Some(media) = article.media_url.clone().filter(|u| !u.is_empty()) else {
                frame_cm.set_status_text("Article has no media URL", 0);
                return;
            };
            let ok = Clipboard::get().set_text(&media);
            frame_cm.set_status_text(
                if ok {
                    "Media URL copied"
                } else {
                    "Copy failed"
                },
                0,
            );
        });
        let state_cf = state.clone();
        let frame_cf = frame;
        let act_copy_feed_url: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_cf.borrow();
            let Some(feed) = st
                .selected_feed
                .as_ref()
                .and_then(|id| st.feeds.iter().find(|f| &f.id == id))
            else {
                frame_cf.set_status_text("No feed selected", 0);
                return;
            };
            let ok = Clipboard::get().set_text(&feed.url);
            frame_cf.set_status_text(if ok { "Feed URL copied" } else { "Copy failed" }, 0);
        });

        // Stop Refresh: cooperative cancel the worker checks
        // between feeds (Python's cancel_refresh).
        let act_stop_refresh: Rc<dyn Fn()> = {
            let state_sr = state.clone();
            let frame_sr = frame;
            Rc::new(move || {
                let st = state_sr.borrow();
                if st.refreshing {
                    st.cancel_refresh
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    frame_sr.set_status_text("Stopping refresh...", 0);
                } else {
                    frame_sr.set_status_text("No refresh is running", 0);
                }
            })
        };

        // Refresh Category: every feed under the selected
        // category node, through the same worker as Refresh All.
        let act_refresh_category: Rc<dyn Fn()> = {
            let state_rc = state.clone();
            let frame_rc = frame;
            let tx_rc = tx.clone();
            Rc::new(move || {
                let work = {
                    let mut st = state_rc.borrow_mut();
                    if st.refreshing {
                        return;
                    }
                    let Some(path) = selected_category(&st) else {
                        frame_rc.set_status_text("Select a category in the tree first", 0);
                        return;
                    };
                    let feeds = feeds_in_category(&st.feeds, &path);
                    if feeds.is_empty() {
                        frame_rc.set_status_text("No feeds in that category", 0);
                        return;
                    }
                    st.refreshing = true;
                    st.cancel_refresh
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    st.refresh_stopped
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    (
                        feeds,
                        st.cancel_refresh.clone(),
                        st.refresh_stopped.clone(),
                        path,
                    )
                };
                let (feeds, cancel, stopped, path) = work;
                frame_rc.set_status_text(
                    &format!("Refreshing {} ({} feeds)...", path, feeds.len()),
                    0,
                );
                announce("start_update", "Refresh Feeds");
                spawn_refresh(feeds, tx_rc.clone(), cancel, stopped);
            })
        };

        // New Category / Delete Category (tree hierarchy ops).
        let act_add_category: Rc<dyn Fn()> = {
            let state_ac = state.clone();
            let frame_ac = frame;
            Rc::new(move || {
                let parent = selected_category(&state_ac.borrow());
                let name = prompt_focus(
                    &frame_ac,
                    "Category name:",
                    "New Category",
                    "",
                    &state_ac.borrow(),
                    &tree,
                    &list,
                );
                let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
                    return;
                };
                let full = match &parent {
                    Some(p) => format!("{p}/{}", name.trim()),
                    None => name.trim().to_string(),
                };
                let mut st = state_ac.borrow_mut();
                match st.db.as_ref().map(|db| db.ensure_category_path(&full)) {
                    Some(Ok(_)) => {
                        st.selected_feed = Some(format!("cat:{full}"));
                        rebuild_tree(&mut st, &tree);
                        refill_articles(&mut st, &list);
                        frame_ac.set_status_text(&format!("Category {full} created"), 0);
                    }
                    Some(Err(e)) => frame_ac.set_status_text(&format!("New category: {e}"), 0),
                    None => frame_ac.set_status_text("No database", 0),
                }
            })
        };
        let act_delete_category: Rc<dyn Fn()> = {
            let state_dc = state.clone();
            let frame_dc = frame;
            Rc::new(move || {
                let Some(path) = selected_category(&state_dc.borrow()) else {
                    frame_dc.set_status_text("Select a category in the tree first", 0);
                    return;
                };
                let dlg = MessageDialog::builder(
                    &frame_dc,
                    &format!(
                        "Delete category {path}? Its feeds and subcategories move up to the parent category."
                    ),
                    "Delete Category",
                )
                .with_style(MessageDialogStyle::YesNo)
                .build();
                let rc = dlg.show_modal();
                restore_focus(&state_dc.borrow(), &tree, &list);
                if rc != ID_YES {
                    return;
                }
                let mut st = state_dc.borrow_mut();
                match st.db.as_ref().map(|db| db.delete_category_path(&path)) {
                    Some(Ok(true)) => {
                        st.selected_feed = None;
                        st.feeds = st
                            .db
                            .as_ref()
                            .and_then(|db| db.feeds().ok())
                            .unwrap_or_default();
                        rebuild_tree(&mut st, &tree);
                        refill_articles(&mut st, &list);
                        frame_dc.set_status_text(&format!("Category {path} deleted"), 0);
                    }
                    Some(Ok(false)) => frame_dc.set_status_text(
                        "Category not deleted (missing, or a name collision blocks it)",
                        0,
                    ),
                    Some(Err(e)) => frame_dc.set_status_text(&format!("Delete category: {e}"), 0),
                    None => frame_dc.set_status_text("No database", 0),
                }
            })
        };

        // Import OPML Here / Export Category OPML.
        let act_import_here: Rc<dyn Fn()> = {
            let state_ih = state.clone();
            let frame_ih = frame;
            Rc::new(move || {
                let Some(path) = selected_category(&state_ih.borrow()) else {
                    frame_ih.set_status_text("Select a category in the tree first", 0);
                    return;
                };
                let dlg = FileDialog::builder(&frame_ih)
                    .with_message("Import OPML into category")
                    .with_wildcard("OPML files (*.opml;*.xml)|*.opml;*.xml")
                    .with_style(FileDialogStyle::Open)
                    .build();
                let rc = dlg.show_modal();
                restore_focus(&state_ih.borrow(), &tree, &list);
                if rc != ID_OK {
                    return;
                }
                let Some(file) = dlg.get_path() else { return };
                let text = match std::fs::read_to_string(&file) {
                    Ok(t) => t,
                    Err(e) => {
                        frame_ih.set_status_text(&format!("Could not read file: {e}"), 0);
                        return;
                    }
                };
                let mut st = state_ih.borrow_mut();
                let Some(db) = st.db.as_ref() else { return };
                match blindrss_core::opml::import_opml_under(db, &text, Some(&path)) {
                    Ok(n) => {
                        st.feeds = db.feeds().unwrap_or_default();
                        rebuild_tree(&mut st, &tree);
                        refill_articles(&mut st, &list);
                        frame_ih.set_status_text(&format!("Imported {n} feeds into {path}"), 0);
                    }
                    Err(e) => frame_ih.set_status_text(&format!("Import failed: {e}"), 0),
                }
            })
        };
        let act_export_category: Rc<dyn Fn()> = {
            let state_ec = state.clone();
            let frame_ec = frame;
            Rc::new(move || {
                let work = {
                    let st = state_ec.borrow();
                    let Some(path) = selected_category(&st) else {
                        frame_ec.set_status_text("Select a category in the tree first", 0);
                        return;
                    };
                    (feeds_in_category(&st.feeds, &path), path)
                };
                let (feeds, path) = work;
                if feeds.is_empty() {
                    frame_ec.set_status_text("No feeds in that category", 0);
                    return;
                }
                let xml = match blindrss_core::opml::export_opml_feeds(&feeds) {
                    Ok(x) => x,
                    Err(e) => {
                        frame_ec.set_status_text(&format!("Export failed: {e}"), 0);
                        return;
                    }
                };
                let dlg = FileDialog::builder(&frame_ec)
                    .with_message("Export category OPML")
                    .with_wildcard("OPML files (*.opml)|*.opml")
                    .with_style(FileDialogStyle::Save)
                    .build();
                let rc = dlg.show_modal();
                restore_focus(&state_ec.borrow(), &tree, &list);
                if rc != ID_OK {
                    return;
                }
                let Some(file) = dlg.get_path() else { return };
                match std::fs::write(&file, xml) {
                    Ok(()) => frame_ec.set_status_text(&format!("Exported {} to {file}", path), 0),
                    Err(e) => frame_ec.set_status_text(&format!("Export failed: {e}"), 0),
                }
            })
        };

        // Feed Errors dialog (Python's "Feeds with Errors" view,
        // fed by the error columns the refresh drain maintains).
        let act_feed_errors: Rc<dyn Fn()> = {
            let state_fe = state.clone();
            let frame_fe = frame;
            Rc::new(move || {
                let errors = {
                    let st = state_fe.borrow();
                    let Some(db) = st.db.as_ref() else { return };
                    db.feed_errors().unwrap_or_default()
                };
                if errors.is_empty() {
                    frame_fe.set_status_text("No feed errors", 0);
                    return;
                }
                let dlg = Dialog::builder(&frame_fe, "Feeds with Errors").build();
                dlg.set_size(Size::new(960, 420));
                let lc = ListCtrl::builder(&dlg)
                    .with_style(ListCtrlStyle::Report)
                    .build();
                lc.insert_column(0, "Feed", ListColumnFormat::Left, 240);
                lc.insert_column(1, "Category", ListColumnFormat::Left, 120);
                lc.insert_column(2, "Failures", ListColumnFormat::Left, 70);
                lc.insert_column(3, "Last error", ListColumnFormat::Left, 360);
                lc.insert_column(4, "Last success", ListColumnFormat::Left, 140);
                for (i, e) in errors.iter().enumerate() {
                    let i = i as i64;
                    lc.insert_item(i, &e.title, None);
                    lc.set_item_text_by_column(i, 1, e.category.as_deref().unwrap_or(""));
                    lc.set_item_text_by_column(i, 2, &e.consecutive_failures.to_string());
                    lc.set_item_text_by_column(i, 3, &e.error);
                    lc.set_item_text_by_column(i, 4, &fmt_ts(e.success_at));
                }
                let close_btn = Button::builder(&dlg).with_label("Close").build();
                let ds = BoxSizer::builder(Orientation::Vertical).build();
                ds.add(&lc, 1, SizerFlag::Expand | SizerFlag::All, 4);
                ds.add(&close_btn, 0, SizerFlag::All, 4);
                dlg.set_sizer(ds, true);
                let dlg_close = dlg;
                close_btn.on_click(move |_| {
                    dlg_close.end_modal(ID_OK);
                });
                dlg.show_modal();
                dlg.destroy();
                restore_focus(&state_fe.borrow(), &tree, &list);
            })
        };

        // View filters (Python's Ctrl+1..6 groups), sort, and
        // find-in-article: one implementation for the View menu
        // and the keyboard dispatch below.
        let filter_action = {
            let state_f = state.clone();
            let frame_f = frame;
            move |group: u8, mode: u8, label: &'static str| -> Rc<dyn Fn()> {
                let state_c = state_f.clone();
                Rc::new(move || {
                    {
                        let mut st = state_c.borrow_mut();
                        if group == 0 {
                            st.read_filter = mode;
                        } else {
                            st.media_filter = mode;
                        }
                        refill_articles(&mut st, &list);
                    }
                    frame_f.set_status_text(label, 0);
                    // Python announces the menu label on filter
                    // changes (issue #67).
                    announce("filter_change", label);
                })
            }
        };
        let act_filter_all = filter_action(0, 0, "All Articles");
        let act_filter_unread = filter_action(0, 1, "Unread Only");
        let act_filter_read = filter_action(0, 2, "Read Only");
        let act_media_all = filter_action(1, 0, "Media and Non-media");
        let act_media_with = filter_action(1, 1, "With Media Only");
        let act_media_without = filter_action(1, 2, "Without Media Only");

        fn persist_sort(st: &State) {
            let path = config_path();
            let mut cfg = blindrss_core::config::Config::load(&path).unwrap_or_default();
            cfg.set_path("article_sort_by", serde_json::json!(st.sort_by));
            cfg.set_path(
                "article_sort_ascending",
                serde_json::json!(st.sort_ascending),
            );
            let _ = cfg.save(&path);
        }
        let sort_action = {
            let state_s = state.clone();
            let frame_s = frame;
            move |key: &'static str, label: &'static str| -> Rc<dyn Fn()> {
                let state_c = state_s.clone();
                Rc::new(move || {
                    {
                        let mut st = state_c.borrow_mut();
                        st.sort_by = key.to_string();
                        persist_sort(&st);
                        refill_articles(&mut st, &list);
                    }
                    frame_s.set_status_text(label, 0);
                })
            }
        };
        let act_sort_date = sort_action("date", "Sort by Date");
        let act_sort_name = sort_action("name", "Sort by Name");
        let act_sort_author = sort_action("author", "Sort by Author");
        let act_sort_feed = sort_action("feed", "Sort by Feed");
        let act_sort_dir: Rc<dyn Fn()> = {
            let state_sd = state.clone();
            let frame_sd = frame;
            Rc::new(move || {
                let ascending = {
                    let mut st = state_sd.borrow_mut();
                    st.sort_ascending = !st.sort_ascending;
                    persist_sort(&st);
                    refill_articles(&mut st, &list);
                    st.sort_ascending
                };
                frame_sd.set_status_text(
                    if ascending {
                        "Sort ascending"
                    } else {
                        "Sort descending"
                    },
                    0,
                );
            })
        };

        fn run_find(reader: &TextCtrl, frame: &Frame, query: &str, from: Option<i64>) {
            let text = reader.get_value();
            let start = from
                .unwrap_or_else(|| reader.get_insertion_point().max(0))
                .max(0) as usize;
            match find_in_text(&text, query, start) {
                Some((a, b)) => {
                    reader.set_selection(a, b);
                    reader.set_insertion_point(b);
                    reader.set_focus();
                    let line = reader
                        .position_to_xy(b)
                        .map(|(_, y)| reader.get_line_text(y))
                        .unwrap_or_default();
                    let line = line.trim();
                    frame.set_status_text(&format!("Found: {line}"), 0);
                    announce("general", &format!("Found: {line}"));
                }
                None => {
                    frame.set_status_text(&format!("Not found: {query}"), 0);
                    announce("general", "Not found");
                }
            }
        }
        let act_find: Rc<dyn Fn()> = {
            let state_fd = state.clone();
            let frame_fd = frame;
            Rc::new(move || {
                let current = state_fd.borrow().find_query.clone();
                let query = prompt_focus(
                    &frame_fd,
                    "Find in article:",
                    "Find",
                    &current,
                    &state_fd.borrow(),
                    &tree,
                    &list,
                );
                let Some(query) = query.filter(|q| !q.trim().is_empty()) else {
                    return;
                };
                state_fd.borrow_mut().find_query = query.clone();
                run_find(&reader, &frame_fd, &query, Some(0));
            })
        };
        let act_find_next: Rc<dyn Fn()> = {
            let state_fn = state.clone();
            let frame_fn = frame;
            let act_find_again = act_find.clone();
            Rc::new(move || {
                let query = state_fn.borrow().find_query.clone();
                if query.is_empty() {
                    act_find_again();
                    return;
                }
                run_find(&reader, &frame_fn, &query, None);
            })
        };

        let file_menu = Menu::builder()
            .append_item(M_ADD_FEED, "&Add Feed...", "Subscribe to a feed")
            .append_item(
                M_IMPORT_OPML,
                "&Import OPML...",
                "Import feeds from an OPML file",
            )
            .append_item(
                M_EXPORT_OPML,
                "&Export OPML...",
                "Export feeds to an OPML file",
            )
            .append_item(
                M_TAKEOUT,
                "Import &YouTube Takeout...",
                "Import subscriptions from a Takeout ZIP",
            )
            .append_separator()
            .append_item(M_BACKUP, "&Backup...", "Save a backup ZIP")
            .append_item(M_RESTORE, "&Restore...", "Restore from a backup ZIP")
            .append_separator()
            .append_item(M_EXIT, "E&xit", "Close BlindRSS")
            .build();
        let feeds_menu = Menu::builder()
            .append_item(M_REFRESH, "&Refresh All", "Refresh every feed")
            .append_item(M_REFRESH_FEED, "Refresh &Feed", "Refresh the selected feed")
            .append_item(
                M_REFRESH_CAT,
                "Refresh &Category",
                "Refresh every feed in the selected category",
            )
            .append_item(M_STOP_REFRESH, "S&top Refresh", "Stop the running refresh")
            .append_item(M_SYNC, "S&ync Providers", "Sync hosted providers")
            .append_item(
                M_COPY_FEED_URL,
                "Copy Feed &URL",
                "Copy the selected feed's URL",
            )
            .append_separator()
            .append_item(M_REMOVE_FEED, "Re&move Feed", "Remove the selected feed")
            .append_item(
                M_EDIT_FEED,
                "&Edit Feed...",
                "Edit the selected feed's title and URL",
            )
            .append_item(
                M_RENAME_CAT,
                "Rename &Category...",
                "Rename the selected feed's category",
            )
            .append_item(
                M_SET_CAT,
                "Set Feed Categor&y...",
                "Move the selected feed to a category",
            )
            .append_separator()
            .append_item(M_ADD_CAT, "New Category...", "Create a category")
            .append_item(
                M_DEL_CAT,
                "Delete Category",
                "Delete the selected category (feeds move up)",
            )
            .append_item(
                M_IMPORT_HERE,
                "Import OPML Here...",
                "Import OPML into the selected category",
            )
            .append_item(
                M_EXPORT_CAT,
                "Export Category OPML...",
                "Export the selected category's feeds",
            )
            .append_separator()
            .append_item(M_NEW_SF, "&New Smart Folder...", "Create a smart folder")
            .append_item(
                M_DEL_SF,
                "&Delete Smart Folder",
                "Delete the selected smart folder",
            )
            .append_item(
                M_ARCHIVE,
                "Rescan &Archive",
                "Rescan the selected feed's podcast archive",
            )
            .build();
        let article_menu = Menu::builder()
            .append_item(
                M_TOGGLE_READ,
                "Toggle &Read",
                "Mark the selected article read or unread",
            )
            .append_item(
                M_FULLTEXT,
                "Read &Full Text",
                "Fetch and extract the full article",
            )
            .append_item(
                M_FAVORITE,
                "Toggle &Favorite",
                "Favorite or unfavorite the selected article",
            )
            .append_separator()
            .append_item(M_MARKALL, "Mark All Read", "Mark every shown article read")
            .append_item(
                M_DELETE_ARTICLE,
                "&Delete Article",
                "Delete the selected article",
            )
            .append_item(
                M_DELETED,
                "De&leted Articles...",
                "View and restore deleted articles",
            )
            .append_separator()
            .append_item(M_PLAY, "&Play Media", "Play the selected article's media")
            .append_item(
                M_DOWNLOAD,
                "Down&load",
                "Download the selected article's media",
            )
            .append_item(M_CAST, "&Cast...", "Cast to a Chromecast")
            .append_separator()
            .append_item(M_COPY_URL, "Copy Article &URL", "Copy the article's link")
            .append_item(
                M_COPY_MEDIA,
                "Copy &Media URL",
                "Copy the article's media link",
            )
            .append_item(M_SEARCH, "&Search", "Search with the current search text")
            .build();
        let view_menu = Menu::builder()
            .append_item(M_FILTER_ALL, "&All Articles", "Show all articles")
            .append_item(M_FILTER_UNREAD, "&Unread Only", "Show unread articles")
            .append_item(M_FILTER_READ, "&Read Only", "Show read articles")
            .append_separator()
            .append_item(
                M_MEDIA_ALL,
                "Media and &Non-media",
                "Show articles with and without media",
            )
            .append_item(M_MEDIA_WITH, "&With Media Only", "Show articles with media")
            .append_item(
                M_MEDIA_WITHOUT,
                "Wit&hout Media Only",
                "Show articles without media",
            )
            .append_separator()
            .append_item(M_SORT_DATE, "Sort by &Date", "Sort articles by date")
            .append_item(M_SORT_NAME, "Sort by &Name", "Sort articles by title")
            .append_item(M_SORT_AUTHOR, "Sort by &Author", "Sort articles by author")
            .append_item(M_SORT_FEED, "Sort by &Feed", "Sort articles by feed")
            .append_item(
                M_SORT_DIR,
                "Toggle &Ascending Sort",
                "Reverse the sort direction",
            )
            .append_separator()
            .append_item(M_FIND, "&Find in Article...", "Search the article text")
            .append_item(M_FIND_NEXT, "Find &Next", "Repeat the last find")
            .build();
        let tools_menu = Menu::builder()
            .append_item(M_SETTINGS, "&Settings...", "Provider accounts and options")
            .append_item(
                M_KEYS,
                "&Keyboard Shortcuts...",
                "View and edit keyboard shortcuts",
            )
            .append_item(
                M_PRUNE,
                "Pr&une Old Articles",
                "Delete read articles older than 30 days",
            )
            .append_item(
                M_UPDATE,
                "Check for &Updates",
                "Check GitHub for a newer release",
            )
            .append_item(
                M_FEED_ERRORS,
                "Feed &Errors...",
                "Feeds whose latest refresh failed",
            )
            .build();
        let help_menu = Menu::builder()
            .append_item(M_HELP, "&User Guide", "Open the BlindRSS user guide")
            .build();
        let menu_bar = MenuBar::builder()
            .append(file_menu, "&File")
            .append(feeds_menu, "&Feeds")
            .append(article_menu, "&Article")
            .append(view_menu, "&View")
            .append(tools_menu, "&Tools")
            .append(help_menu, "&Help")
            .build();
        frame.set_menu_bar(menu_bar);
        let menu_actions: Vec<(i32, Rc<dyn Fn()>)> = vec![
            (M_ADD_FEED, act_add.clone()),
            (M_IMPORT_OPML, act_import.clone()),
            (M_EXPORT_OPML, act_export.clone()),
            (M_TAKEOUT, act_takeout.clone()),
            (M_BACKUP, act_backup.clone()),
            (M_RESTORE, act_restore.clone()),
            (M_REFRESH, act_refresh.clone()),
            (M_REFRESH_FEED, act_refresh_feed.clone()),
            (M_REFRESH_CAT, act_refresh_category.clone()),
            (M_STOP_REFRESH, act_stop_refresh.clone()),
            (M_ADD_CAT, act_add_category.clone()),
            (M_DEL_CAT, act_delete_category.clone()),
            (M_IMPORT_HERE, act_import_here.clone()),
            (M_EXPORT_CAT, act_export_category.clone()),
            (M_FEED_ERRORS, act_feed_errors.clone()),
            (M_SYNC, act_sync.clone()),
            (M_REMOVE_FEED, act_remove.clone()),
            (M_EDIT_FEED, act_edit_feed.clone()),
            (M_RENAME_CAT, act_rename_cat.clone()),
            (M_SET_CAT, act_set_cat.clone()),
            (M_NEW_SF, act_new_sf.clone()),
            (M_DEL_SF, act_del_sf.clone()),
            (M_ARCHIVE, act_archive.clone()),
            (M_TOGGLE_READ, act_toggle_read.clone()),
            (M_FULLTEXT, act_fulltext.clone()),
            (M_FAVORITE, act_fav.clone()),
            (M_MARKALL, act_markall.clone()),
            (M_DELETE_ARTICLE, act_delete.clone()),
            (M_DELETED, act_deleted.clone()),
            (M_PLAY, act_play.clone()),
            (M_DOWNLOAD, act_download.clone()),
            (M_CAST, act_cast.clone()),
            (M_SEARCH, act_search.clone()),
            (M_SETTINGS, act_settings.clone()),
            (M_KEYS, act_keys.clone()),
            (M_PRUNE, act_prune.clone()),
            (M_UPDATE, act_update.clone()),
            (M_HELP, act_help.clone()),
            (M_FILTER_ALL, act_filter_all.clone()),
            (M_FILTER_UNREAD, act_filter_unread.clone()),
            (M_FILTER_READ, act_filter_read.clone()),
            (M_MEDIA_ALL, act_media_all.clone()),
            (M_MEDIA_WITH, act_media_with.clone()),
            (M_MEDIA_WITHOUT, act_media_without.clone()),
            (M_SORT_DATE, act_sort_date.clone()),
            (M_SORT_NAME, act_sort_name.clone()),
            (M_SORT_AUTHOR, act_sort_author.clone()),
            (M_SORT_FEED, act_sort_feed.clone()),
            (M_SORT_DIR, act_sort_dir.clone()),
            (M_FIND, act_find.clone()),
            (M_FIND_NEXT, act_find_next.clone()),
            (M_COPY_URL, act_copy_url.clone()),
            (M_COPY_MEDIA, act_copy_media.clone()),
            (M_COPY_FEED_URL, act_copy_feed_url.clone()),
        ];
        let frame_menu = frame;
        frame.on_menu(move |ev| {
            let id = ev.get_id();
            if id == M_EXIT {
                frame_menu.close(true);
                return;
            }
            if let Some((_, action)) = menu_actions.iter().find(|(mid, _)| *mid == id) {
                action();
            }
        });

        // Context menus (PARITY 5C): right-click / Menu key on the
        // feed tree and the article list. Selections dispatch through
        // the frame's menu handler above (wx command events from a
        // popup propagate to the frame), so the same actions run.
        tree.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            let mut menu = Menu::builder()
                .append_item(M_REFRESH_FEED, "Refresh &Feed", "")
                .append_item(M_REFRESH_CAT, "Refresh &Category", "")
                .append_item(M_REFRESH, "&Refresh All", "")
                .append_item(M_STOP_REFRESH, "S&top Refresh", "")
                .append_separator()
                .append_item(M_ADD_CAT, "New &Category...", "")
                .append_item(M_DEL_CAT, "&Delete Category", "")
                .append_item(M_IMPORT_HERE, "Import OPML &Here...", "")
                .append_item(M_EXPORT_CAT, "&Export Category OPML...", "")
                .append_separator()
                .append_item(M_EDIT_FEED, "&Edit Feed...", "")
                .append_item(M_REMOVE_FEED, "Re&move Feed", "")
                .append_item(M_RENAME_CAT, "Rename &Category...", "")
                .append_item(M_SET_CAT, "Set Feed Categor&y...", "")
                .append_item(M_ARCHIVE, "Rescan &Archive", "")
                .append_item(M_COPY_FEED_URL, "Copy Feed &URL", "")
                .append_separator()
                .append_item(M_NEW_SF, "&New Smart Folder...", "")
                .append_item(M_DEL_SF, "&Delete Smart Folder", "")
                .build();
            tree.popup_menu(&mut menu, None);
        });
        list.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            let mut menu = Menu::builder()
                .append_item(M_TOGGLE_READ, "Toggle &Read", "")
                .append_item(M_FULLTEXT, "Read &Full Text", "")
                .append_item(M_FAVORITE, "Toggle &Favorite", "")
                .append_separator()
                .append_item(M_PLAY, "&Play Media", "")
                .append_item(M_DOWNLOAD, "Down&load", "")
                .append_item(M_CAST, "&Cast...", "")
                .append_separator()
                .append_item(M_COPY_URL, "Copy Article &URL", "")
                .append_item(M_COPY_MEDIA, "Copy &Media URL", "")
                .append_separator()
                .append_item(M_DELETE_ARTICLE, "&Delete Article", "")
                .append_item(M_MARKALL, "Mark All Read", "")
                .build();
            list.popup_menu(&mut menu, None);
        });

        // Enter (item activation): play when the article has
        // media, otherwise open its link in the browser
        // (Brandon's rule, 2026-10-30).
        let state_enter = state.clone();
        let frame_enter = frame;
        let play_enter = act_play.clone();
        list.bind_internal(EventType::LIST_ITEM_ACTIVATED, move |ev| {
            ev.skip(false);
            let target = {
                let st = state_enter.borrow();
                st.selected_article
                    .and_then(|i| st.articles.get(i))
                    .map(|a| (a.media_url.clone(), a.url.clone()))
            };
            match target {
                Some((Some(media), _)) if !media.is_empty() => play_enter(),
                Some((_, url)) if !url.is_empty() => open_in_browser(&url),
                _ => frame_enter.set_status_text("Article has no media or link", 0),
            }
        });

        // Keyboard dispatch (PARITY 7.5): a frame-level char hook
        // resolves keys through the shortcut registry (defaults +
        // user overrides), exactly Python's model. Handled keys are
        // consumed; everything else skips through to the control.
        {
            let state_hook = state.clone();
            let keymap_hook = keymap.clone();
            let h_refresh = act_refresh.clone();
            let h_stop = act_stop_refresh.clone();
            let h_refresh_feed = act_refresh_feed.clone();
            let h_add = act_add.clone();
            let h_errors = act_feed_errors.clone();
            let h_toggle_read = act_toggle_read.clone();
            let h_fav = act_fav.clone();
            let h_play = act_play.clone();
            let h_f_all = act_filter_all.clone();
            let h_f_unread = act_filter_unread.clone();
            let h_f_read = act_filter_read.clone();
            let h_m_all = act_media_all.clone();
            let h_m_with = act_media_with.clone();
            let h_m_without = act_media_without.clone();
            let h_s_date = act_sort_date.clone();
            let h_s_name = act_sort_name.clone();
            let h_s_author = act_sort_author.clone();
            let h_s_feed = act_sort_feed.clone();
            let h_s_dir = act_sort_dir.clone();
            let h_find = act_find.clone();
            let h_find_next = act_find_next.clone();
            frame.bind_internal(EventType::CHAR_HOOK, move |ev| {
                let code = ev.get_key_code().unwrap_or(0);
                let ctrl = ev.control_down() || ev.cmd_down();
                let shift = ev.shift_down();
                if ev.alt_down() {
                    ev.skip(true);
                    return;
                }
                // Plain Backspace in the article list toggles
                // read state (Python's binding); elsewhere it is
                // ordinary editing/navigation.
                if code == 8 && !ctrl && !shift {
                    let in_list = {
                        let st = state_hook.borrow();
                        st.focus_list && !st.search_focused
                    };
                    if in_list {
                        h_toggle_read();
                        ev.skip(false);
                    } else {
                        ev.skip(true);
                    }
                    return;
                }
                let key_name: Option<String> = if (340..=351).contains(&code) {
                    Some(format!("F{}", code - 339))
                } else {
                    let uni = ev.get_unicode_key().unwrap_or(0);
                    let c = if (32..127).contains(&uni) { uni } else { code };
                    if (33..127).contains(&c) {
                        char::from_u32(c as u32).map(|ch| ch.to_uppercase().collect::<String>())
                    } else {
                        None
                    }
                };
                let Some(key_name) = key_name else {
                    ev.skip(true);
                    return;
                };
                let mut combo = String::new();
                if ctrl {
                    combo.push_str("Ctrl+");
                }
                if shift {
                    combo.push_str("Shift+");
                }
                combo.push_str(&key_name);
                // Find pair: reader-local, not registry commands.
                if combo == "Ctrl+F" {
                    h_find();
                    ev.skip(false);
                    return;
                }
                if combo == "F3" || combo == "Ctrl+G" {
                    h_find_next();
                    ev.skip(false);
                    return;
                }
                let cmd = keymap_hook.borrow().get(&combo).cloned();
                let handled = match cmd.as_deref() {
                    Some("feeds.refresh_all") => {
                        h_refresh();
                        true
                    }
                    Some("feeds.stop_refresh") => {
                        h_stop();
                        true
                    }
                    Some("feeds.refresh_selected") => {
                        h_refresh_feed();
                        true
                    }
                    Some("feeds.add") => {
                        h_add();
                        true
                    }
                    Some("feeds.view_errors") => {
                        h_errors();
                        true
                    }
                    Some("article.toggle_favorite") => {
                        h_fav();
                        true
                    }
                    Some("player.play_pause") => {
                        h_play();
                        true
                    }
                    Some("filter.read_all") => {
                        h_f_all();
                        true
                    }
                    Some("filter.read_unread") => {
                        h_f_unread();
                        true
                    }
                    Some("filter.read_read") => {
                        h_f_read();
                        true
                    }
                    Some("filter.media_all") => {
                        h_m_all();
                        true
                    }
                    Some("filter.media_with") => {
                        h_m_with();
                        true
                    }
                    Some("filter.media_without") => {
                        h_m_without();
                        true
                    }
                    Some("sort.date") => {
                        h_s_date();
                        true
                    }
                    Some("sort.name") => {
                        h_s_name();
                        true
                    }
                    Some("sort.author") => {
                        h_s_author();
                        true
                    }
                    Some("sort.feed") => {
                        h_s_feed();
                        true
                    }
                    Some("sort.ascending") => {
                        h_s_dir();
                        true
                    }
                    Some("view.focus_search") => {
                        search_box.set_focus();
                        true
                    }
                    _ => false,
                };
                ev.skip(!handled);
            });
        }

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
                                Ok(n) => {
                                    new_total += n;
                                    let _ = db.clear_feed_error(&feed.id);
                                }
                                Err(e) => frame_tick.set_status_text(
                                    &format!("{}: store failed: {e}", feed.title),
                                    0,
                                ),
                            }
                        }
                    }
                    Ok(RefreshMsg::FeedDone(feed, Err(e))) => {
                        let st = state_tick.borrow();
                        if let Some(db) = st.db.as_ref() {
                            let _ = db.record_feed_error(&feed.id, &e);
                        }
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
                st.last_refresh_end = Some(std::time::Instant::now());
                let stopped = st
                    .refresh_stopped
                    .load(std::sync::atomic::Ordering::Relaxed);
                st.refresh_stopped
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                st.cancel_refresh
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                rebuild_tree(&mut st, &tree);
                refill_articles(&mut st, &list);
                if stopped {
                    frame_tick.set_status_text("Refresh stopped", 0);
                    announce("stop_update", "Refresh stopped");
                } else {
                    frame_tick
                        .set_status_text(&format!("Refresh complete: {new_total} new articles"), 0);
                    announce(
                        "general",
                        &format!("Refresh complete: {new_total} new articles"),
                    );
                }
            }
        });
        timer.start(150, false);

        let sched_timer = Box::leak(Box::new(Timer::new(&frame)));
        let state_sched = state.clone();
        let refresh_sched = act_refresh.clone();
        sched_timer.on_tick(move |_| {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let interval = cfg.get_i64("refresh_interval", 300);
            if interval <= 0 {
                return;
            }
            let due = {
                let mut st = state_sched.borrow_mut();
                if st.refreshing {
                    return;
                }
                match st.last_refresh_end {
                    // First tick sets the baseline; the startup
                    // refresh (below) covers the initial fetch.
                    None => {
                        st.last_refresh_end = Some(std::time::Instant::now());
                        false
                    }
                    Some(t) => t.elapsed().as_secs() >= interval as u64,
                }
            };
            if due {
                refresh_sched();
            }
        });
        sched_timer.start(60_000, false);

        // Python parity: start_maximized opens the window
        // maximized (Python skips this only when starting hidden
        // in the tray; the Rust tray has no restore path yet).
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if cfg.get_bool("start_maximized", false) {
                frame.maximize(true);
            }
        }
        frame.centre();
        frame.show(true);
        // Python `_focus_default_control`, ported faithfully: Python
        // schedules it with wx.CallAfter from __init__ (the frame is
        // shown later by main.py) and the handler checks IsShown
        // before tree.SetFocus(). A synchronous set_focus during
        // window creation loses to the activation that follows, so
        // defer it past the show/activation. wxdragon::call_after
        // requires Send closures and widget handles are not Send, so
        // a one-shot timer is the deferral mechanism here — it fires
        // on the main thread after activation, like CallAfter.
        let focus_timer = Box::leak(Box::new(Timer::new(&frame)));
        focus_timer.on_tick(move |_| {
            if frame.is_shown() {
                tree.set_focus();
            }
        });
        focus_timer.start(75, true);

        // Startup refresh: Python's refresh_on_startup defaults
        // to on. One-shot, shortly after the window is up.
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if cfg.get_bool("refresh_on_startup", true) {
                let startup_timer = Box::leak(Box::new(Timer::new(&frame)));
                let refresh_start = act_refresh.clone();
                startup_timer.on_tick(move |_| {
                    refresh_start();
                });
                startup_timer.start(1200, true);
            }
        }
    });
}
