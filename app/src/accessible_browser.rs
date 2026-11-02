//! The Accessible Browser window (Python
//! `gui/accessibility.py`'s AccessibleBrowserFrame): a
//! VoiceOver-first front end with a flattened Views list
//! (categories expand/collapse in place), a paged Articles
//! list, and a reader pane fed by the core arbitration
//! (`accessible::extract_article_body`) behind a 300-entry
//! full-text LRU with read-ahead prefetch.
//!
//! Actions that belong to the main window (add feed, OPML,
//! settings, the play queue, downloads) run through
//! `AbCallbacks`, which the main window installs at startup;
//! unset callbacks are no-ops so the window still works
//! standalone. Read/favorite/delete state changes go through
//! the same helpers the main window uses.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use blindrss_core::accessible as ab;
use blindrss_core::db::{Article, Db};
use wxdragon::prelude::*;

const PAGE_SIZE: usize = 400;
const PREFETCH_AHEAD: usize = 8;

/// Main-window delegates (Python's `_delegate` targets).
/// A no-argument main-window command (Python's menu delegates).
pub type AbAction = Option<Rc<dyn Fn()>>;
/// A main-window command acting on one article.
pub type AbArticleAction = Option<Rc<dyn Fn(&Article)>>;
/// A main-window command acting on a view/feed/category id.
pub type AbStrAction = Option<Rc<dyn Fn(&str)>>;

#[derive(Clone, Default)]
pub struct AbCallbacks {
    pub add_feed: AbAction,
    pub open_article_url: AbAction,
    pub detect_page_feeds: AbAction,
    pub add_category: AbAction,
    pub new_smart_folder: AbAction,
    pub import_opml: AbAction,
    pub import_takeout: AbAction,
    pub export_opml: AbAction,
    pub refresh_feeds: AbAction,
    pub stop_refresh: AbAction,
    pub settings: AbAction,
    pub toggle_player: AbAction,
    pub play_queue: AbAction,
    pub play_queue_next: AbAction,
    pub play_queue_prev: AbAction,
    pub equalizer: AbAction,
    pub persistent_search: AbAction,
    pub filter_rules: AbAction,
    pub find_feed: AbAction,
    pub podcast_archive: AbAction,
    pub video_search: AbAction,
    pub import_cookies: AbAction,
    pub shortcuts: AbAction,
    pub feed_errors: AbAction,
    pub check_updates: AbAction,
    pub about: AbAction,
    pub download_article: AbArticleAction,
    pub view_history: AbArticleAction,
    pub mark_view_read: AbStrAction,
    pub remove_feed: AbStrAction,
    pub remove_category: AbStrAction,
    pub toggle_feed_notifications: AbStrAction,
    pub copy_feed_url: AbStrAction,
}

fn call(cb: &AbAction) {
    if let Some(f) = cb {
        f();
    }
}

enum AbMsg {
    ArticlesPage {
        view_id: String,
        offset: usize,
        page: Vec<Article>,
        total: i64,
    },
    ArticlesFailed {
        view_id: String,
        error: String,
    },
    Body {
        art_id: String,
        token: u64,
        body: Option<String>,
        cacheable: bool,
    },
    Chapters {
        art_id: String,
        token: u64,
        chapters: Vec<(f64, String, String)>,
    },
    Rich {
        art_id: String,
        token: u64,
        html: Option<String>,
        url: String,
    },
    PlayResolved {
        article: Box<Article>,
        url: Option<String>,
        queue: Vec<Article>,
        index: usize,
    },
    Prefetched {
        art_id: String,
        body: Option<String>,
        cacheable: bool,
    },
    RefreshDone,
}

struct AbState {
    callbacks: AbCallbacks,
    entries: Vec<ab::AccessibleViewEntry>,
    expanded: HashSet<String>,
    known_categories: HashSet<String>,
    current_view: String,
    base_articles: Vec<Article>,
    current_articles: Vec<Article>,
    total: Option<i64>,
    loading: bool,
    content_token: u64,
    body_cache: blindrss_core::reader_cache::ReaderCache,
    cached_ids: HashSet<String>,
    chapter_cache: HashMap<String, Vec<(f64, String, String)>>,
    chapter_inflight: HashSet<String>,
    current_body_art: String,
    current_body_text: String,
    feed_titles: HashMap<String, String>,
}

type Shared = Rc<RefCell<AbState>>;

/// Read-ahead work queue drained by two worker threads
/// (Python runs three and serializes provider fetches; the
/// provider fetch here carries its own lock).
struct PrefetchQueue {
    queue: Mutex<VecDeque<(String, Article)>>,
    inflight: Mutex<HashSet<String>>,
}

#[derive(Clone)]
struct Widgets {
    frame: Frame,
    refresh_btn: Button,
    expand_btn: Button,
    collapse_btn: Button,
    open_btn: Button,
    mark_read_btn: Button,
    mark_unread_btn: Button,
    view_list: ListBox,
    article_list: ListBox,
    content_ctrl: TextCtrl,
    status_lbl: StaticText,
    search_ctrl: TextCtrl,
    download_btn: Button,
    load_more_btn: Button,
    rich_chk: CheckBox,
    web: Option<wxdragon::widgets::webview::WebView>,
}

thread_local! {
    /// The one open Accessible Browser frame (UI thread only,
    /// like the rest of this window's state).
    static OPEN_FRAME: RefCell<Option<Frame>> = const { RefCell::new(None) };
}

/// Whether an Accessible Browser frame is currently open.
pub fn is_open() -> bool {
    OPEN_FRAME.with(|cell| cell.borrow().is_some())
}

/// Raise the open Accessible Browser, if any.
pub fn raise_existing() {
    OPEN_FRAME.with(|cell| {
        if let Some(frame) = *cell.borrow() {
            frame.show(true);
            frame.raise();
        }
    });
}

pub fn open_accessible_browser(parent: &Frame, callbacks: AbCallbacks) {
    let frame = Frame::builder()
        .with_parent(parent)
        .with_title(&crate::tr("BlindRSS Accessible Browser"))
        .with_size(Size::new(1280, 820))
        .build();

    let (tx, rx) = std::sync::mpsc::channel::<AbMsg>();
    let state: Shared = Rc::new(RefCell::new(AbState {
        callbacks,
        entries: Vec::new(),
        expanded: HashSet::new(),
        known_categories: HashSet::new(),
        current_view: String::new(),
        base_articles: Vec::new(),
        current_articles: Vec::new(),
        total: None,
        loading: false,
        content_token: 0,
        body_cache: blindrss_core::reader_cache::ReaderCache::new(300, 8_000_000),
        cached_ids: HashSet::new(),
        chapter_cache: HashMap::new(),
        chapter_inflight: HashSet::new(),
        current_body_art: String::new(),
        current_body_text: String::new(),
        feed_titles: HashMap::new(),
    }));

    // ---- layout -------------------------------------------------
    let root = BoxSizer::builder(Orientation::Vertical).build();
    let intro = StaticText::builder(&frame)
        .with_label(&crate::tr(
            "VoiceOver-friendly browser for feeds, articles, and content. Use the lists below to choose a view and article.",
        ))
        .build();
    root.add(&intro, 0, SizerFlag::All | SizerFlag::Expand, 8);

    let toolbar = BoxSizer::builder(Orientation::Horizontal).build();
    let mk_btn = |label: &str, name: &str| {
        let b = Button::builder(&frame)
            .with_label(&crate::tr(label))
            .build();
        crate::msaa_names::set_name(&b, &crate::tr(name));
        toolbar.add(&b, 0, SizerFlag::Right, 6);
        b
    };
    let refresh_btn = mk_btn("Refresh Feeds", "Refresh Feeds");
    let load_more_btn = mk_btn("Load More Articles", "Load More Articles");
    let expand_btn = mk_btn("Expand Category", "Expand Category");
    let collapse_btn = mk_btn("Collapse Category", "Collapse Category");
    let open_btn = mk_btn("Open or Play Article", "Open or Play Article");
    let mark_read_btn = mk_btn("Mark Read", "Mark Read");
    let mark_unread_btn = mk_btn("Mark Unread", "Mark Unread");
    let download_btn = mk_btn("Download", "Download Article");
    download_btn.enable(false);
    let rich_chk = CheckBox::builder(&frame)
        .with_label(&crate::tr("Rich View"))
        .build();
    crate::msaa_names::set_name(&rich_chk, &crate::tr("Rich Full-Text View"));
    rich_chk.set_value(rich_enabled());
    toolbar.add(&rich_chk, 0, SizerFlag::AlignCenterVertical, 0);
    root.add_sizer(
        &toolbar,
        0,
        SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom,
        8,
    );

    let search_row = BoxSizer::builder(Orientation::Horizontal).build();
    let search_lbl = StaticText::builder(&frame)
        .with_label(&crate::tr("Filter Articles:"))
        .build();
    search_row.add(
        &search_lbl,
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::Right,
        6,
    );
    let search_ctrl = TextCtrl::builder(&frame).build();
    crate::msaa_names::set_name(&search_ctrl, &crate::tr("Accessible Article Filter"));
    search_row.add(&search_ctrl, 1, SizerFlag::Expand, 0);
    root.add_sizer(
        &search_row,
        0,
        SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom | SizerFlag::Expand,
        8,
    );

    let content = BoxSizer::builder(Orientation::Horizontal).build();
    let left = BoxSizer::builder(Orientation::Vertical).build();
    let views_lbl = StaticText::builder(&frame)
        .with_label(&crate::tr("Views"))
        .build();
    left.add(&views_lbl, 0, SizerFlag::Bottom, 4);
    let view_list = ListBox::builder(&frame).build();
    crate::msaa_names::set_name(&view_list, &crate::tr("Accessible Views"));
    view_list.set_min_size(Size::new(240, 240));
    left.add(&view_list, 1, SizerFlag::Expand, 0);
    let hint = StaticText::builder(&frame)
        .with_label(&crate::tr(
            "Categories can be expanded or collapsed. Use the buttons, or use Right Arrow to expand and Left Arrow to collapse.",
        ))
        .build();
    left.add(&hint, 0, SizerFlag::Top, 6);
    content.add_sizer(&left, 1, SizerFlag::All | SizerFlag::Expand, 8);

    let middle = BoxSizer::builder(Orientation::Vertical).build();
    let articles_lbl = StaticText::builder(&frame)
        .with_label(&crate::tr("Articles"))
        .build();
    middle.add(&articles_lbl, 0, SizerFlag::Bottom, 4);
    let article_list = ListBox::builder(&frame).build();
    crate::msaa_names::set_name(&article_list, &crate::tr("Accessible Articles"));
    article_list.set_min_size(Size::new(260, 240));
    middle.add(&article_list, 1, SizerFlag::Expand, 0);
    let status_lbl = StaticText::builder(&frame)
        .with_label(&crate::tr("Choose a view to load articles."))
        .build();
    middle.add(&status_lbl, 0, SizerFlag::Top, 6);
    content.add_sizer(&middle, 1, SizerFlag::All | SizerFlag::Expand, 8);

    let right = BoxSizer::builder(Orientation::Vertical).build();
    let article_lbl = StaticText::builder(&frame)
        .with_label(&crate::tr("Article Content"))
        .build();
    right.add(&article_lbl, 0, SizerFlag::Bottom, 4);
    let content_ctrl = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    crate::msaa_names::set_name(&content_ctrl, &crate::tr("Accessible Article Content"));
    content_ctrl.set_min_size(Size::new(620, 240));
    right.add(&content_ctrl, 1, SizerFlag::Expand, 0);
    let web = wxdragon::widgets::webview::WebView::is_backend_available(
        wxdragon::widgets::webview::WebViewBackend::Default,
    )
    .then(|| wxdragon::widgets::webview::WebView::builder(&frame).build());
    if let Some(view) = &web {
        right.add(view, 1, SizerFlag::Expand, 0);
        view.show(false);
    }
    content.add_sizer(&right, 3, SizerFlag::All | SizerFlag::Expand, 8);
    root.add_sizer(&content, 1, SizerFlag::Expand, 0);
    frame.set_sizer(root, true);

    let widgets = Widgets {
        frame,
        refresh_btn,
        expand_btn,
        collapse_btn,
        open_btn,
        mark_read_btn,
        mark_unread_btn,
        view_list,
        article_list,
        content_ctrl,
        status_lbl,
        search_ctrl,
        download_btn,
        load_more_btn,
        rich_chk,
        web,
    };

    build_menus(&widgets, &state);
    wire_events(&widgets, &state, tx.clone());
    start_prefetch_workers(tx.clone());

    // Message drain (150 ms), the app-wide pattern.
    let timer = Box::leak(Box::new(Timer::new(&frame)));
    {
        let state = state.clone();
        let widgets = widgets.clone();
        timer.on_tick(move |_| {
            while let Ok(msg) = rx.try_recv() {
                handle_msg(&widgets, &state, msg);
            }
        });
    }
    timer.start(150, false);
    OPEN_FRAME.with(|cell| *cell.borrow_mut() = Some(frame));
    frame.on_close(move |event| {
        timer.stop();
        OPEN_FRAME.with(|cell| *cell.borrow_mut() = None);
        event.skip(true);
    });

    refresh_views(&widgets, &state);
    frame.show(true);
    frame.raise();
}

fn rich_enabled() -> bool {
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    cfg.get_bool("full_text_rich_view", false)
}

fn db() -> Option<Db> {
    Db::open(&crate::db_path()).ok()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- views ----------------------------------------------------

fn refresh_views(w: &Widgets, state: &Shared) {
    let Some(db) = db() else { return };
    let feeds = db.feeds().unwrap_or_default();
    let mut feed_titles: HashMap<String, String> = HashMap::new();
    let mut feed_rows: Vec<ab::AccessibleFeed> = Vec::new();
    for f in &feeds {
        feed_titles.insert(f.id.clone(), f.title.clone());
        feed_rows.push(ab::AccessibleFeed {
            id: &f.id,
            title: &f.title,
            category: f.category.as_deref().unwrap_or(""),
            unread_count: db.unread_count(Some(&f.id)).unwrap_or(0),
        });
    }
    let cats = db.categories().unwrap_or_default();
    let cat_titles: Vec<&str> = cats.iter().map(|c| c.title.as_str()).collect();
    // Hierarchy from the paths themselves: a category's parent
    // is its path prefix when that prefix is also a category.
    let mut hierarchy: Vec<(&str, &str)> = Vec::new();
    for c in &cat_titles {
        if let Some(pos) = c.rfind('/') {
            let parent = &c[..pos];
            if cat_titles.iter().any(|t| t == &parent) {
                hierarchy.push((c, parent));
            }
        }
    }
    let entries = ab::build_accessible_view_entries(&feed_rows, &cat_titles, &hierarchy, true);
    let mut st = state.borrow_mut();
    st.feed_titles = feed_titles;
    // Python _sync_expanded_categories: first sync expands
    // everything; later, keep the set, drop vanished, add new.
    let cat_names: HashSet<String> = entries
        .iter()
        .filter(|e| e.kind == "category")
        .map(|e| e.cat_name.clone())
        .collect();
    if st.known_categories.is_empty() {
        st.expanded = cat_names.clone();
    } else {
        st.expanded.retain(|c| cat_names.contains(c));
        for c in &cat_names {
            if !st.known_categories.contains(c) {
                st.expanded.insert(c.clone());
            }
        }
    }
    st.known_categories = cat_names;
    st.entries = entries;
    drop(st);
    refill_view_list(w, state, true);
}

fn refill_view_list(w: &Widgets, state: &Shared, keep_selection: bool) {
    let st = state.borrow();
    let expanded: Vec<&str> = st.expanded.iter().map(|s| s.as_str()).collect();
    let visible = ab::visible_accessible_view_entries(&st.entries, &expanded);
    let labels: Vec<String> = visible
        .iter()
        .map(|e| ab::format_accessible_view_label(e, &expanded))
        .collect();
    let current = st.current_view.clone();
    drop(st);
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    w.view_list.set_items(&label_refs);
    if keep_selection && !current.is_empty() {
        let st = state.borrow();
        let expanded: Vec<&str> = st.expanded.iter().map(|s| s.as_str()).collect();
        let visible = ab::visible_accessible_view_entries(&st.entries, &expanded);
        if let Some(pos) = visible.iter().position(|e| e.view_id == current) {
            w.view_list.set_selection(pos as u32, true);
        }
    }
}

fn selected_view_entry(w: &Widgets, state: &Shared) -> Option<ab::AccessibleViewEntry> {
    let sel = w.view_list.get_selection()? as usize;
    let st = state.borrow();
    let expanded: Vec<&str> = st.expanded.iter().map(|s| s.as_str()).collect();
    let visible = ab::visible_accessible_view_entries(&st.entries, &expanded);
    visible.get(sel).cloned()
}

// ---- article loading ------------------------------------------

fn load_view(w: &Widgets, state: &Shared, tx: &Sender<AbMsg>, view_id: &str) {
    {
        let mut st = state.borrow_mut();
        if st.loading {
            return;
        }
        st.loading = true;
        st.current_view = view_id.to_string();
        st.base_articles.clear();
        st.current_articles.clear();
        st.total = None;
    }
    w.article_list
        .set_items(&[crate::tr("Loading articles...").as_str()]);
    w.content_ctrl.set_value("");
    w.status_lbl.set_label(&crate::tr("Loading articles..."));
    spawn_page(tx.clone(), view_id.to_string(), 0);
}

fn spawn_page(tx: Sender<AbMsg>, view_id: String, offset: usize) {
    std::thread::spawn(move || {
        let msg = match db() {
            Some(db) => match db.articles_page(&view_id, offset, PAGE_SIZE) {
                Ok((page, total)) => AbMsg::ArticlesPage {
                    view_id,
                    offset,
                    page,
                    total,
                },
                Err(e) => AbMsg::ArticlesFailed {
                    view_id,
                    error: e.to_string(),
                },
            },
            None => AbMsg::ArticlesFailed {
                view_id,
                error: "database unavailable".into(),
            },
        };
        let _ = tx.send(msg);
    });
}

fn article_label(st: &AbState, a: &Article) -> String {
    let feed_title = st
        .feed_titles
        .get(&a.feed_id)
        .map(|s| s.as_str())
        .unwrap_or("");
    ab::article_list_label(
        &a.title,
        feed_title,
        a.author.as_deref().unwrap_or(""),
        &ab::humanize_article_date(a.date.as_deref().unwrap_or(""), now_unix()),
        a.is_read,
    )
}

fn search_text(st: &AbState, a: &Article) -> String {
    let feed_title = st
        .feed_titles
        .get(&a.feed_id)
        .map(|s| s.as_str())
        .unwrap_or("");
    format!(
        "{} {} {} {} {}",
        a.title,
        feed_title,
        a.author.as_deref().unwrap_or(""),
        a.url,
        blindrss_core::extract::html_to_text(a.content.as_deref().unwrap_or(""))
    )
}

fn apply_filter(w: &Widgets, state: &Shared) {
    let query = w.search_ctrl.get_value();
    let selected_id = selected_article(w, state).map(|a| a.id);
    let mut st = state.borrow_mut();
    st.current_articles = st
        .base_articles
        .iter()
        .filter(|a| ab::article_matches_filter(&search_text(&st, a), &query))
        .cloned()
        .collect();
    let labels: Vec<String> = st
        .current_articles
        .iter()
        .map(|a| article_label(&st, a))
        .collect();
    drop(st);
    if labels.is_empty() {
        w.article_list
            .set_items(&[crate::tr("No articles found.").as_str()]);
        w.content_ctrl.set_value("");
        w.download_btn.enable(false);
        return;
    }
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    w.article_list.set_items(&refs);
    let st = state.borrow();
    let new_idx = selected_id
        .as_ref()
        .and_then(|id| st.current_articles.iter().position(|a| &a.id == id))
        .unwrap_or(0);
    drop(st);
    w.article_list.set_selection(new_idx as u32, true);
    if selected_id.is_none()
        || selected_id.as_deref()
            != state
                .borrow()
                .current_articles
                .get(new_idx)
                .map(|a| a.id.as_str())
    {
        show_article(w, state, new_idx);
    }
}

fn selected_article(w: &Widgets, state: &Shared) -> Option<Article> {
    let sel = w.article_list.get_selection()? as usize;
    state.borrow().current_articles.get(sel).cloned()
}

// ---- reader ---------------------------------------------------

fn compose(st: &AbState, a: &Article, body: &str) -> String {
    let mut text = ab::article_header(
        &a.title,
        &ab::humanize_article_date(a.date.as_deref().unwrap_or(""), now_unix()),
        a.author.as_deref().unwrap_or(""),
        &a.url,
    );
    text.push_str(body);
    if let Some(chapters) = st.chapter_cache.get(&a.id) {
        let refs: Vec<(f64, &str, &str)> = chapters
            .iter()
            .map(|(s, t, h)| (*s, t.as_str(), h.as_str()))
            .collect();
        text = format!(
            "{}\n\n{}\n",
            text.trim_end(),
            ab::format_accessible_chapters(&refs)
        );
    } else if st.chapter_inflight.contains(&a.id) {
        text = format!(
            "{}\n\n{}\n",
            text.trim_end(),
            crate::tr("Chapter availability: Loading.")
        );
    }
    text
}

fn set_content(w: &Widgets, state: &Shared, a: &Article, body: &str) {
    let mut st = state.borrow_mut();
    st.current_body_art = a.id.clone();
    st.current_body_text = body.to_string();
    let text = compose(&st, a, body);
    drop(st);
    w.content_ctrl.set_value(&text);
}

fn show_article(w: &Widgets, state: &Shared, idx: usize) {
    let article = {
        let st = state.borrow();
        st.current_articles.get(idx).cloned()
    };
    let Some(article) = article else {
        w.download_btn.enable(false);
        return;
    };
    let token = {
        let mut st = state.borrow_mut();
        st.content_token += 1;
        st.content_token
    };
    w.download_btn
        .enable(article.media_url.as_deref().is_some_and(|u| !u.is_empty()));
    // Cached body wins instantly; otherwise the feed snippet
    // shows while the full text loads (Python's model).
    let cached = state.borrow_mut().body_cache.get(&article.id, false);
    if let Some((body, _src)) = cached {
        set_content(w, state, &article, &body);
    } else {
        let snippet = blindrss_core::postprocess::strip_leading_boilerplate(
            &blindrss_core::extract::html_to_text(article.content.as_deref().unwrap_or("")),
        );
        set_content(w, state, &article, &snippet);
        spawn_body_load(article.clone(), token);
    }
    spawn_chapter_load(w, state, &article, token);
    if rich_active(w, state) {
        spawn_rich_load(article.clone(), token);
    } else {
        enqueue_prefetch(state, idx);
    }
}

fn rich_active(w: &Widgets, state: &Shared) -> bool {
    let _ = state;
    w.rich_chk.is_checked() && w.web.is_some()
}

fn feed_encoding(feed_id: &str) -> String {
    let Some(db) = db() else { return String::new() };
    db.feed_settings(feed_id)
        .ok()
        .and_then(|s| {
            s.get("encoding")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

fn keep_alt_for(feed_id: &str) -> bool {
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    if let Some(db) = db()
        && let Ok(Some(over)) = db.feed_show_images(feed_id)
    {
        return over;
    }
    cfg.get_bool("show_image_alt", false)
}

/// The provider server-side fetch for the arbitration
/// (Python's provider_fetch): Miniflux fetch-content for
/// `miniflux-entry:<id>` articles while Miniflux is the
/// active provider. Serialized — the provider session is not
/// guaranteed thread-safe (Python's provider lock).
fn miniflux_provider_fetch(article_id: &str, _url: &str) -> Option<String> {
    static PROVIDER_LOCK: Mutex<()> = Mutex::new(());
    let entry: i64 = article_id.strip_prefix("miniflux-entry:")?.parse().ok()?;
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).ok()?;
    if blindrss_core::provider::active_name(&cfg) != "miniflux" {
        return None;
    }
    let section = cfg.raw().get("providers")?.get("miniflux")?;
    let url = section.get("url")?.as_str()?;
    let key = section.get("api_key")?.as_str()?;
    if url.is_empty() || key.is_empty() {
        return None;
    }
    let _guard = PROVIDER_LOCK.lock().ok()?;
    blindrss_core::providers::MinifluxClient::new(url, key)
        .fetch_content_remote(entry)
        .ok()
}

fn load_body(article: &Article) -> (Option<String>, bool) {
    let input = ab::ArticleBodyInput {
        id: &article.id,
        url: &article.url,
        content_html: article.content.as_deref().unwrap_or(""),
        title: &article.title,
        author: article.author.as_deref().unwrap_or(""),
    };
    let encoding = feed_encoding(&article.feed_id);
    let keep_alt = keep_alt_for(&article.feed_id);
    ab::extract_article_body(&input, Some(&miniflux_provider_fetch), &encoding, keep_alt)
}

fn spawn_body_load(article: Article, token: u64) {
    BODY_TX.with(|cell| {
        let Some(tx) = cell.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let (body, cacheable) = load_body(&article);
            let _ = tx.send(AbMsg::Body {
                art_id: article.id.clone(),
                token,
                body,
                cacheable,
            });
        });
    });
}

fn spawn_chapter_load(w: &Widgets, state: &Shared, article: &Article, token: u64) {
    {
        let st = state.borrow();
        if st.chapter_cache.contains_key(&article.id) || st.chapter_inflight.contains(&article.id) {
            return;
        }
    }
    let can_have = article.media_url.as_deref().is_some_and(|u| !u.is_empty())
        || blindrss_core::ytfull::is_youtube_video_url(&article.url);
    if !can_have {
        return;
    }
    state
        .borrow_mut()
        .chapter_inflight
        .insert(article.id.clone());
    // Recompose so the "Loading" chapter line appears.
    let body = state.borrow().current_body_text.clone();
    if state.borrow().current_body_art == article.id {
        set_content(w, state, article, &body);
    }
    let art_id = article.id.clone();
    BODY_TX.with(|cell| {
        let Some(tx) = cell.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let chapters: Vec<(f64, String, String)> = db()
                .and_then(|d| d.chapters(&art_id).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|c| {
                    (
                        c.start,
                        c.title.unwrap_or_default(),
                        c.href.unwrap_or_default(),
                    )
                })
                .collect();
            let _ = tx.send(AbMsg::Chapters {
                art_id,
                token,
                chapters,
            });
        });
    });
}

fn spawn_rich_load(article: Article, token: u64) {
    BODY_TX.with(|cell| {
        let Some(tx) = cell.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let url = article.url.clone();
            let fallback = article
                .content
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| article.description.clone())
                .unwrap_or_default();
            let date = article
                .date
                .clone()
                .unwrap_or_default()
                .split('T')
                .next()
                .unwrap_or("")
                .to_string();
            let jar = blindrss_core::cookies::load_jar_beside(&crate::config_path());
            let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
            let html = blindrss_core::article_html::render_rich_article(
                &url,
                &fallback,
                &article.title,
                article.author.as_deref().unwrap_or(""),
                &date,
                &header,
                "",
                "",
                &crate::ui_language(),
            )
            .map(|f| crate::rich_document(&f));
            let _ = tx.send(AbMsg::Rich {
                art_id: article.id.clone(),
                token,
                html,
                url,
            });
        });
    });
}

// ---- prefetch ---------------------------------------------------

thread_local! {
    static BODY_TX: RefCell<Option<Sender<AbMsg>>> = const { RefCell::new(None) };
    static PREFETCH: RefCell<Option<Arc<PrefetchQueue>>> = const { RefCell::new(None) };
}

fn start_prefetch_workers(tx: Sender<AbMsg>) {
    BODY_TX.with(|cell| *cell.borrow_mut() = Some(tx.clone()));
    let queue = Arc::new(PrefetchQueue {
        queue: Mutex::new(VecDeque::new()),
        inflight: Mutex::new(HashSet::new()),
    });
    PREFETCH.with(|cell| *cell.borrow_mut() = Some(queue.clone()));
    for _ in 0..2 {
        let queue = queue.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                let item = queue
                    .queue
                    .lock()
                    .map(|mut q| q.pop_front())
                    .unwrap_or(None);
                let Some((art_id, article)) = item else {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    continue;
                };
                let (body, cacheable) = load_body(&article);
                if let Ok(mut inf) = queue.inflight.lock() {
                    inf.remove(&art_id);
                }
                let _ = tx.send(AbMsg::Prefetched {
                    art_id,
                    body,
                    cacheable,
                });
            }
        });
    }
}

fn enqueue_prefetch(state: &Shared, idx: usize) {
    PREFETCH.with(|cell| {
        let Some(queue) = cell.borrow().clone() else {
            return;
        };
        let st = state.borrow();
        if let Ok(mut q) = queue.queue.lock() {
            q.clear();
        }
        let mut inf = match queue.inflight.lock() {
            Ok(i) => i,
            Err(_) => return,
        };
        for a in st
            .current_articles
            .iter()
            .skip(idx + 1)
            .take(PREFETCH_AHEAD)
        {
            if st.cached_ids.contains(&a.id) || inf.contains(&a.id) {
                continue;
            }
            inf.insert(a.id.clone());
            if let Ok(mut q) = queue.queue.lock() {
                q.push_back((a.id.clone(), a.clone()));
            }
        }
    });
}

// ---- menus ------------------------------------------------------

const M_ADD_FEED: i32 = 7101;
const M_OPEN_URL: i32 = 7102;
const M_DETECT_PAGE: i32 = 7103;
const M_ADD_CATEGORY: i32 = 7104;
const M_NEW_SMART: i32 = 7105;
const M_IMPORT_OPML: i32 = 7106;
const M_IMPORT_TAKEOUT: i32 = 7107;
const M_EXPORT_OPML: i32 = 7108;
const M_REFRESH: i32 = 7109;
const M_STOP_REFRESH: i32 = 7110;
const M_MARK_VIEW_READ: i32 = 7111;
const M_SETTINGS: i32 = 7112;
const M_CLOSE: i32 = 7113;
const M_OPEN_PLAY: i32 = 7120;
const M_OPEN_BROWSER: i32 = 7121;
const M_COPY_LINK: i32 = 7122;
const M_COPY_TEXT: i32 = 7123;
const M_COPY_MEDIA: i32 = 7124;
const M_FAVORITE: i32 = 7125;
const M_MARK_READ: i32 = 7126;
const M_MARK_UNREAD: i32 = 7127;
const M_DELETE: i32 = 7128;
const M_DOWNLOAD: i32 = 7129;
const M_DETECT_AUDIO: i32 = 7130;
const M_FIND: i32 = 7131;
const M_HISTORY: i32 = 7132;
const M_FEED_DESC: i32 = 7133;
const M_PLAYER_TOGGLE: i32 = 7140;
const M_PLAY_QUEUE: i32 = 7141;
const M_QUEUE_NEXT: i32 = 7142;
const M_QUEUE_PREV: i32 = 7143;
const M_EQUALIZER: i32 = 7144;
const M_PERSISTENT_SEARCH: i32 = 7150;
const M_FILTER_RULES: i32 = 7151;
const M_FIND_FEED: i32 = 7152;
const M_ARCHIVE: i32 = 7153;
const M_VIDEO_SEARCH: i32 = 7154;
const M_IMPORT_COOKIES: i32 = 7155;
const M_SHORTCUTS: i32 = 7160;
const M_FEED_ERRORS: i32 = 7161;
const M_UPDATES: i32 = 7162;
const M_ABOUT: i32 = 7163;
const M_CTX_REFRESH_FEED: i32 = 7180;
const M_CTX_MARK_VIEW_READ: i32 = 7181;
const M_CTX_COPY_FEED_URL: i32 = 7182;
const M_CTX_TOGGLE_NOTIF: i32 = 7183;
const M_CTX_REMOVE_FEED: i32 = 7184;
const M_CTX_REMOVE_CATEGORY: i32 = 7185;
const M_CTX_NEW_SMART: i32 = 7186;
const M_CTX_EXPAND: i32 = 7187;
const M_CTX_COLLAPSE: i32 = 7188;

fn build_menus(w: &Widgets, state: &Shared) {
    let tr = |s: &str| crate::tr(s);
    let file_menu = Menu::builder()
        .append_item(M_ADD_FEED, &tr("&Add Feed...\tCtrl+N"), "")
        .append_item(M_OPEN_URL, &tr("&Open Article..."), "")
        .append_item(M_DETECT_PAGE, &tr("Detect Feeds on &Page..."), "")
        .append_item(M_ADD_CATEGORY, &tr("Add &Category..."), "")
        .append_item(M_NEW_SMART, &tr("New Smart F&older..."), "")
        .append_item(M_IMPORT_OPML, &tr("&Import OPML..."), "")
        .append_item(M_IMPORT_TAKEOUT, &tr("Import &YouTube Takeout..."), "")
        .append_item(M_EXPORT_OPML, &tr("&Export OPML..."), "")
        .append_separator()
        .append_item(M_REFRESH, &tr("&Refresh Feeds\tCtrl+R"), "")
        .append_item(M_STOP_REFRESH, &tr("&Stop Refresh"), "")
        .append_item(
            M_MARK_VIEW_READ,
            &tr("Mark All in Current View as R&ead"),
            "",
        )
        .append_separator()
        .append_item(M_SETTINGS, &tr("&Settings..."), "")
        .append_item(M_CLOSE, &tr("Close &Window\tCtrl+W"), "")
        .build();
    let article_menu = Menu::builder()
        .append_item(M_OPEN_PLAY, &tr("&Open or Play"), "")
        .append_item(M_OPEN_BROWSER, &tr("Open in &Browser"), "")
        .append_separator()
        .append_item(M_COPY_LINK, &tr("Copy &Link"), "")
        .append_item(M_COPY_TEXT, &tr("Copy &Text"), "")
        .append_item(M_COPY_MEDIA, &tr("Copy &Media Link"), "")
        .append_separator()
        .append_item(M_FAVORITE, &tr("Toggle &Favorite\tCtrl+D"), "")
        .append_item(M_MARK_READ, &tr("Mark &Read"), "")
        .append_item(M_MARK_UNREAD, &tr("Mark &Unread"), "")
        .append_item(M_DELETE, &tr("De&lete"), "")
        .append_separator()
        .append_item(M_DOWNLOAD, &tr("&Download"), "")
        .append_item(M_DETECT_AUDIO, &tr("Detect &Audio"), "")
        .append_item(M_FIND, &tr("&Find in Article...\tCtrl+F"), "")
        .append_item(M_HISTORY, &tr("View &History..."), "")
        .append_item(M_FEED_DESC, &tr("View Feed De&scription..."), "")
        .build();
    let player_menu = Menu::builder()
        .append_item(M_PLAYER_TOGGLE, &tr("&Show or Hide Player"), "")
        .append_item(M_PLAY_QUEUE, &tr("Open Play &Queue..."), "")
        .append_item(M_QUEUE_NEXT, &tr("Play &Next in Queue"), "")
        .append_item(M_QUEUE_PREV, &tr("Play &Previous in Queue"), "")
        .append_item(M_EQUALIZER, &tr("&Equalizer..."), "")
        .build();
    let tools_menu = Menu::builder()
        .append_item(M_PERSISTENT_SEARCH, &tr("&Persistent Search..."), "")
        .append_item(M_FILTER_RULES, &tr("&Filter Rules..."), "")
        .append_item(M_FIND_FEED, &tr("Find a Podcast or &RSS..."), "")
        .append_item(M_ARCHIVE, &tr("Podcast &Archive..."), "")
        .append_item(M_VIDEO_SEARCH, &tr("&Video Search..."), "")
        .append_item(M_IMPORT_COOKIES, &tr("Import Site &Cookies..."), "")
        .build();
    let help_menu = Menu::builder()
        .append_item(M_SHORTCUTS, &tr("&Keyboard Shortcuts..."), "")
        .append_item(M_FEED_ERRORS, &tr("View Feed &Errors..."), "")
        .append_item(M_UPDATES, &tr("Check for &Updates..."), "")
        .append_item(M_ABOUT, &tr("&About"), "")
        .build();
    let bar = MenuBar::builder()
        .append(file_menu, &tr("&File"))
        .append(article_menu, &tr("&Article"))
        .append(player_menu, &tr("&Player"))
        .append(tools_menu, &tr("&Tools"))
        .append(help_menu, &tr("&Help"))
        .build();
    w.frame.set_menu_bar(bar);

    let w2 = w.clone();
    let state2 = state.clone();
    w.frame.on_menu(move |event| {
        handle_menu(&w2, &state2, event.get_id());
    });
}

fn handle_menu(w: &Widgets, state: &Shared, id: i32) {
    let cb = state.borrow().callbacks.clone();
    match id {
        M_ADD_FEED => call(&cb.add_feed),
        M_OPEN_URL => call(&cb.open_article_url),
        M_DETECT_PAGE => call(&cb.detect_page_feeds),
        M_ADD_CATEGORY => call(&cb.add_category),
        M_NEW_SMART | M_CTX_NEW_SMART => call(&cb.new_smart_folder),
        M_IMPORT_OPML => call(&cb.import_opml),
        M_IMPORT_TAKEOUT => call(&cb.import_takeout),
        M_EXPORT_OPML => call(&cb.export_opml),
        M_REFRESH => act_refresh(w, state),
        M_STOP_REFRESH => call(&cb.stop_refresh),
        M_MARK_VIEW_READ | M_CTX_MARK_VIEW_READ => {
            let view = state.borrow().current_view.clone();
            if let Some(f) = &cb.mark_view_read {
                f(&view);
            }
        }
        M_SETTINGS => call(&cb.settings),
        M_CLOSE => {
            w.frame.close(true);
        }
        M_OPEN_PLAY | M_DETECT_AUDIO => act_open_play(w, state),
        M_OPEN_BROWSER => {
            if let Some(a) = selected_article(w, state)
                && !a.url.is_empty()
            {
                crate::open_in_browser(&a.url);
            }
        }
        M_COPY_LINK => {
            if let Some(a) = selected_article(w, state)
                && !a.url.is_empty()
            {
                let _ = Clipboard::get().set_text(&a.url);
            }
        }
        M_COPY_TEXT => {
            let text = w.content_ctrl.get_value();
            if !text.is_empty() {
                let _ = Clipboard::get().set_text(&text);
            }
        }
        M_COPY_MEDIA => {
            if let Some(a) = selected_article(w, state)
                && let Some(u) = a.media_url.as_deref().filter(|u| !u.is_empty())
            {
                let _ = Clipboard::get().set_text(u);
            }
        }
        M_FAVORITE => act_toggle_favorite(w, state),
        M_MARK_READ => act_set_read(w, state, true),
        M_MARK_UNREAD => act_set_read(w, state, false),
        M_DELETE => act_delete(w, state),
        M_DOWNLOAD => {
            if let Some(a) = selected_article(w, state)
                && let Some(f) = &cb.download_article
            {
                f(&a);
            }
        }
        M_FIND => act_find_in_article(w, state),
        M_HISTORY => {
            if let Some(a) = selected_article(w, state)
                && let Some(f) = &cb.view_history
            {
                f(&a);
            }
        }
        M_FEED_DESC => act_feed_description(w, state),
        M_PLAYER_TOGGLE => call(&cb.toggle_player),
        M_PLAY_QUEUE => call(&cb.play_queue),
        M_QUEUE_NEXT => call(&cb.play_queue_next),
        M_QUEUE_PREV => call(&cb.play_queue_prev),
        M_EQUALIZER => call(&cb.equalizer),
        M_PERSISTENT_SEARCH => call(&cb.persistent_search),
        M_FILTER_RULES => call(&cb.filter_rules),
        M_FIND_FEED => call(&cb.find_feed),
        M_ARCHIVE => call(&cb.podcast_archive),
        M_VIDEO_SEARCH => call(&cb.video_search),
        M_IMPORT_COOKIES => call(&cb.import_cookies),
        M_SHORTCUTS => call(&cb.shortcuts),
        M_FEED_ERRORS => call(&cb.feed_errors),
        M_UPDATES => call(&cb.check_updates),
        M_ABOUT => call(&cb.about),
        M_CTX_REFRESH_FEED => act_refresh_selected_feed(w, state),
        M_CTX_COPY_FEED_URL => {
            if let Some(entry) = selected_view_entry(w, state)
                && let Some(f) = &cb.copy_feed_url
            {
                f(&entry.view_id);
            }
        }
        M_CTX_TOGGLE_NOTIF => {
            if let Some(entry) = selected_view_entry(w, state)
                && let Some(f) = &cb.toggle_feed_notifications
            {
                f(&entry.view_id);
            }
        }
        M_CTX_REMOVE_FEED => {
            if let Some(entry) = selected_view_entry(w, state)
                && let Some(f) = &cb.remove_feed
            {
                f(&entry.view_id);
            }
        }
        M_CTX_REMOVE_CATEGORY => {
            if let Some(entry) = selected_view_entry(w, state)
                && let Some(f) = &cb.remove_category
            {
                f(&entry.cat_name);
            }
        }
        M_CTX_EXPAND => set_category_expanded(w, state, true),
        M_CTX_COLLAPSE => set_category_expanded(w, state, false),
        _ => {}
    }
}

// ---- events -----------------------------------------------------

fn wire_events(w: &Widgets, state: &Shared, tx: Sender<AbMsg>) {
    // Receivers are Copy handles; closures move their own
    // Widgets clone (borrowing the receiver's owner while
    // moving it into the handler does not compile).
    {
        let wc = w.clone();
        let state = state.clone();
        w.refresh_btn.on_click(move |_| act_refresh(&wc, &state));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let tx = tx.clone();
        w.load_more_btn
            .on_click(move |_| act_load_more(&wc, &state, &tx));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        w.expand_btn
            .on_click(move |_| set_category_expanded(&wc, &state, true));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        w.collapse_btn
            .on_click(move |_| set_category_expanded(&wc, &state, false));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        w.open_btn.on_click(move |_| act_open_play(&wc, &state));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        w.mark_read_btn
            .on_click(move |_| act_set_read(&wc, &state, true));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        w.mark_unread_btn
            .on_click(move |_| act_set_read(&wc, &state, false));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let btn = w.download_btn;
        btn.on_click(move |_| {
            if let Some(a) = selected_article(&wc, &state) {
                let cb = state.borrow().callbacks.download_article.clone();
                if let Some(f) = cb {
                    f(&a);
                }
            }
        });
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let tx = tx.clone();
        let list = w.view_list;
        list.on_selection_changed(move |_| {
            if let Some(entry) = selected_view_entry(&wc, &state)
                && entry.kind != "category"
            {
                load_view(&wc, &state, &tx, &entry.view_id);
            }
        });
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let list = w.article_list;
        list.on_selection_changed(move |_| {
            if let Some(sel) = wc.article_list.get_selection() {
                show_article(&wc, &state, sel as usize);
            }
        });
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let list = w.article_list;
        list.on_item_double_clicked(move |_| {
            act_open_play(&wc, &state);
        });
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let ctrl = w.search_ctrl;
        ctrl.on_text_updated(move |_| apply_filter(&wc, &state));
    }
    {
        let wc = w.clone();
        let state = state.clone();
        let chk = w.rich_chk;
        chk.on_toggled(move |ev| {
            apply_reader_mode(&wc, &state, ev.is_checked());
        });
    }
    // View list arrows: Right expands, Left collapses the
    // selected category (Python's on_view_list_key_down).
    {
        let wc = w.clone();
        let state = state.clone();
        let list = w.view_list;
        list.bind_internal(EventType::KEY_DOWN, move |ev| {
            let code = ev.get_key_code().unwrap_or(0);
            match code {
                316 => {
                    set_category_expanded(&wc, &state, true);
                    ev.skip(false);
                }
                314 => {
                    set_category_expanded(&wc, &state, false);
                    ev.skip(false);
                }
                _ => ev.skip(true),
            }
        });
    }
    // Frame char hook: the window's own commands.
    {
        let wc = w.clone();
        let state = state.clone();
        let frame = w.frame;
        frame.bind_internal(EventType::CHAR_HOOK, move |ev| {
            let code = ev.get_key_code().unwrap_or(0);
            let ctrl = ev.control_down() || ev.cmd_down();
            match (code, ctrl) {
                (87, true) => {
                    wc.frame.close(true);
                    ev.skip(false);
                }
                (82, true) => {
                    act_refresh(&wc, &state);
                    ev.skip(false);
                }
                (70, true) => {
                    act_find_in_article(&wc, &state);
                    ev.skip(false);
                }
                (68, true) => {
                    act_toggle_favorite(&wc, &state);
                    ev.skip(false);
                }
                (127, false) => {
                    act_delete(&wc, &state);
                    ev.skip(false);
                }
                _ => ev.skip(true),
            }
        });
    }
    // Context menus.
    {
        let wc = w.clone();
        let state = state.clone();
        let list = w.view_list;
        list.bind_internal(EventType::CONTEXT_MENU, move |_| {
            let Some(entry) = selected_view_entry(&wc, &state) else {
                return;
            };
            let tr = |s: &str| crate::tr(s);
            let mut menu = match entry.kind.as_str() {
                "feed" => Menu::builder()
                    .append_item(M_CTX_REFRESH_FEED, &tr("Refresh Feed"), "")
                    .append_item(
                        M_CTX_MARK_VIEW_READ,
                        &tr("Mark All in Current View as Read"),
                        "",
                    )
                    .append_item(M_CTX_COPY_FEED_URL, &tr("Copy Feed URL"), "")
                    .append_item(M_CTX_TOGGLE_NOTIF, &tr("Toggle Notifications"), "")
                    .append_item(M_CTX_REMOVE_FEED, &tr("Remove Feed"), "")
                    .build(),
                "category" => Menu::builder()
                    .append_item(M_CTX_EXPAND, &tr("Expand Category"), "")
                    .append_item(M_CTX_COLLAPSE, &tr("Collapse Category"), "")
                    .append_item(
                        M_CTX_MARK_VIEW_READ,
                        &tr("Mark All in Current View as Read"),
                        "",
                    )
                    .append_item(M_CTX_REMOVE_CATEGORY, &tr("Remove Category"), "")
                    .build(),
                _ => Menu::builder()
                    .append_item(
                        M_CTX_MARK_VIEW_READ,
                        &tr("Mark All in Current View as Read"),
                        "",
                    )
                    .append_item(M_CTX_NEW_SMART, &tr("New Smart Folder..."), "")
                    .build(),
            };
            wc.view_list.popup_menu(&mut menu, None);
        });
    }
    {
        let wc = w.clone();
        let list = w.article_list;
        list.bind_internal(EventType::CONTEXT_MENU, move |_| {
            let tr = |s: &str| crate::tr(s);
            let mut menu = Menu::builder()
                .append_item(M_OPEN_PLAY, &tr("Open or Play"), "")
                .append_item(M_OPEN_BROWSER, &tr("Open in Browser"), "")
                .append_separator()
                .append_item(M_COPY_LINK, &tr("Copy Link"), "")
                .append_item(M_COPY_TEXT, &tr("Copy Text"), "")
                .append_item(M_COPY_MEDIA, &tr("Copy Media Link"), "")
                .append_separator()
                .append_item(M_FAVORITE, &tr("Toggle Favorite"), "")
                .append_item(M_MARK_READ, &tr("Mark Read"), "")
                .append_item(M_MARK_UNREAD, &tr("Mark Unread"), "")
                .append_item(M_DELETE, &tr("Delete"), "")
                .append_separator()
                .append_item(M_DOWNLOAD, &tr("Download"), "")
                .append_item(M_FEED_DESC, &tr("View Feed Description..."), "")
                .build();
            wc.article_list.popup_menu(&mut menu, None);
        });
    }
    // Rich-view navigation policy (Python rich_view_links):
    // external links leave the WebView for the browser flow.
    if let Some(view) = &w.web {
        use wxdragon::event::webview_events::WebViewEvents;
        let frame = w.frame;
        view.on_navigating(move |event| {
            if !event.get_target().unwrap_or_default().is_empty() {
                return;
            }
            if let Some(url) = event.get_url()
                && !crate::rich_embed_url(&url)
                && let Some(ext) = crate::rich_external_url(&url)
            {
                event.event.event.veto();
                crate::open_article_link(&frame, &ext);
            }
        });
        let frame = w.frame;
        view.on_new_window(move |event| {
            if let Some(url) = event.get_url().and_then(|u| crate::rich_external_url(&u)) {
                crate::open_article_link(&frame, &url);
            }
        });
    }
}

// ---- actions ------------------------------------------------------

fn act_refresh(w: &Widgets, state: &Shared) {
    let cb = state.borrow().callbacks.refresh_feeds.clone();
    call(&cb);
    refresh_views(w, state);
}

fn act_refresh_selected_feed(w: &Widgets, state: &Shared) {
    let Some(entry) = selected_view_entry(w, state) else {
        return;
    };
    if entry.kind != "feed" {
        return;
    }
    let feed_id = entry.view_id.clone();
    BODY_TX.with(|cell| {
        let Some(tx) = cell.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            if let Some(db) = db()
                && let Some(feed) = db
                    .feeds()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|f| f.id == feed_id)
            {
                let _ = blindrss_core::refresh::refresh_feed(&db, &feed);
            }
            let _ = tx.send(AbMsg::RefreshDone);
        });
    });
    let _ = state;
}

fn act_load_more(_w: &Widgets, state: &Shared, tx: &Sender<AbMsg>) {
    let (view, offset, more) = {
        let st = state.borrow();
        let loaded = st.base_articles.len();
        let more = st.total.map(|t| (loaded as i64) < t).unwrap_or(false);
        (st.current_view.clone(), loaded, more)
    };
    if more && !view.is_empty() {
        spawn_page(tx.clone(), view, offset);
    }
}

fn set_category_expanded(w: &Widgets, state: &Shared, expanded: bool) {
    let Some(entry) = selected_view_entry(w, state) else {
        return;
    };
    if entry.kind != "category" {
        return;
    }
    {
        let mut st = state.borrow_mut();
        if expanded {
            st.expanded.insert(entry.cat_name.clone());
        } else {
            st.expanded.remove(&entry.cat_name);
        }
    }
    refill_view_list(w, state, true);
}

fn act_set_read(w: &Widgets, state: &Shared, read: bool) {
    let Some(article) = selected_article(w, state) else {
        return;
    };
    let Some(db) = db() else { return };
    crate::mark_article_read(&db, &article, read);
    crate::propagate_read_state(&article, read);
    let mut st = state.borrow_mut();
    for a in st.base_articles.iter_mut() {
        if a.id == article.id {
            a.is_read = read;
        }
    }
    for a in st.current_articles.iter_mut() {
        if a.id == article.id {
            a.is_read = read;
        }
    }
    drop(st);
    refresh_article_labels(w, state);
    refresh_views(w, state);
}

fn act_toggle_favorite(w: &Widgets, state: &Shared) {
    let Some(article) = selected_article(w, state) else {
        return;
    };
    let Some(db) = db() else { return };
    let new_value = !article.is_favorite;
    crate::mark_article_favorite(&db, &article, new_value);
    let mut st = state.borrow_mut();
    for a in st.base_articles.iter_mut() {
        if a.id == article.id {
            a.is_favorite = new_value;
        }
    }
    for a in st.current_articles.iter_mut() {
        if a.id == article.id {
            a.is_favorite = new_value;
        }
    }
    drop(st);
    refresh_article_labels(w, state);
}

fn act_delete(w: &Widgets, state: &Shared) {
    let Some(article) = selected_article(w, state) else {
        return;
    };
    let Some(db) = db() else { return };
    let _ = db.delete_article(&article.id);
    let mut st = state.borrow_mut();
    st.base_articles.retain(|a| a.id != article.id);
    if let Some(t) = st.total {
        st.total = Some(t - 1);
    }
    drop(st);
    apply_filter(w, state);
}

fn refresh_article_labels(w: &Widgets, state: &Shared) {
    let st = state.borrow();
    let labels: Vec<String> = st
        .current_articles
        .iter()
        .map(|a| article_label(&st, a))
        .collect();
    drop(st);
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let sel = w.article_list.get_selection();
    w.article_list.set_items(&refs);
    if let Some(sel) = sel {
        w.article_list.set_selection(sel, true);
    }
}

fn act_open_play(w: &Widgets, state: &Shared) {
    let Some(article) = selected_article(w, state) else {
        return;
    };
    // Mark read on activation, like the main window.
    if !article.is_read {
        if let Some(db) = db() {
            crate::mark_article_read(&db, &article, true);
            crate::propagate_read_state(&article, true);
        }
        let mut st = state.borrow_mut();
        for a in st.base_articles.iter_mut() {
            if a.id == article.id {
                a.is_read = true;
            }
        }
        for a in st.current_articles.iter_mut() {
            if a.id == article.id {
                a.is_read = true;
            }
        }
        drop(st);
        refresh_article_labels(w, state);
    }
    let playable = blindrss_core::download::downloaded_media_path(
        &blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default(),
        &article,
    )
    .is_some()
        || article.media_url.as_deref().is_some_and(|u| {
            !u.is_empty()
                && blindrss_core::media::is_playable_media(u, article.media_type.as_deref())
        })
        || blindrss_core::media::is_ytdlp_supported(&article.url)
        || article.content.as_deref().is_some_and(|html| {
            blindrss_core::media::detect_media_in_page(html, &article.url).is_some()
        });
    if !playable {
        // Text article: open the standalone reader with the
        // body this window already computed.
        let body = state.borrow().current_body_text.clone();
        let body = if body.is_empty() {
            blindrss_core::extract::html_to_text(article.content.as_deref().unwrap_or(""))
        } else {
            body
        };
        crate::article_window::open(&article.title, &body, Some(&article.url));
        return;
    }
    let queue: Vec<Article> = state.borrow().current_articles.clone();
    let index = queue.iter().position(|a| a.id == article.id).unwrap_or(0);
    BODY_TX.with(|cell| {
        let Some(tx) = cell.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            let url = blindrss_core::media::resolve_article_for_play(&cfg, &article)
                .ok()
                .map(|r| r.url);
            let _ = tx.send(AbMsg::PlayResolved {
                article: Box::new(article),
                url,
                queue,
                index,
            });
        });
    });
}

fn act_find_in_article(w: &Widgets, state: &Shared) {
    let _ = state;
    let entry = TextEntryDialog::builder(&w.frame, "Find in article:", "Find").build();
    if entry.show_modal() != 5100 {
        return;
    }
    let needle = entry.get_value().unwrap_or_default();
    if needle.is_empty() {
        return;
    }
    let text = w.content_ctrl.get_value();
    if let Some(pos) = text.to_lowercase().find(&needle.to_lowercase()) {
        w.content_ctrl.set_focus();
        w.content_ctrl
            .set_selection(pos as i64, (pos + needle.chars().count()) as i64);
    } else {
        w.status_lbl
            .set_label(&format!("{}: {}", crate::tr("Not found"), needle));
    }
}

fn act_feed_description(w: &Widgets, state: &Shared) {
    let Some(article) = selected_article(w, state) else {
        return;
    };
    let feed_title = state
        .borrow()
        .feed_titles
        .get(&article.feed_id)
        .cloned()
        .unwrap_or_default();
    let desc = article.description.as_deref().unwrap_or("");
    let body = if desc.trim().is_empty() {
        crate::tr("No description is available for this feed.")
    } else {
        blindrss_core::extract::html_to_text(desc)
    };
    crate::article_window::open(&feed_title, &body, None);
}

fn apply_reader_mode(w: &Widgets, state: &Shared, on: bool) {
    // Persist the shared setting (Python gates both windows on
    // full_text_rich_view).
    let mut cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    cfg.set_path("full_text_rich_view", serde_json::json!(on));
    let _ = cfg.save(&crate::config_path());
    if let Some(view) = &w.web {
        view.show(on);
    }
    w.content_ctrl.show(!on);
    w.frame.layout();
    if on && let Some(article) = selected_article(w, state) {
        let token = state.borrow().content_token;
        spawn_rich_load(article, token);
    }
}

// ---- message handling ---------------------------------------------

fn handle_msg(w: &Widgets, state: &Shared, msg: AbMsg) {
    match msg {
        AbMsg::ArticlesPage {
            view_id,
            offset,
            page,
            total,
        } => {
            {
                let mut st = state.borrow_mut();
                if view_id != st.current_view {
                    return;
                }
                st.loading = false;
                if offset == 0 {
                    st.base_articles = page;
                } else {
                    let have: HashSet<String> =
                        st.base_articles.iter().map(|a| a.id.clone()).collect();
                    st.base_articles
                        .extend(page.into_iter().filter(|a| !have.contains(&a.id)));
                }
                st.total = Some(total);
                let status = if total >= 0 {
                    format!(
                        "{}: {} of {}.",
                        crate::tr("Articles loaded"),
                        st.base_articles.len(),
                        total
                    )
                } else {
                    format!(
                        "{}: {}.",
                        crate::tr("Articles loaded"),
                        st.base_articles.len()
                    )
                };
                drop(st);
                w.status_lbl.set_label(&status);
            }
            let loaded = state.borrow().base_articles.len();
            let total = state.borrow().total.unwrap_or(0);
            w.load_more_btn.enable((loaded as i64) < total);
            apply_filter(w, state);
        }
        AbMsg::ArticlesFailed { view_id, error } => {
            let mut st = state.borrow_mut();
            if view_id != st.current_view {
                return;
            }
            st.loading = false;
            drop(st);
            w.article_list
                .set_items(&[crate::tr("Failed to load articles.").as_str()]);
            w.status_lbl.set_label(&format!(
                "{}: {error}",
                crate::tr("Failed to load articles")
            ));
        }
        AbMsg::Body {
            art_id,
            token,
            body,
            cacheable,
        } => {
            let Some(body) = body else { return };
            {
                let mut st = state.borrow_mut();
                if cacheable {
                    st.cached_ids.insert(art_id.clone());
                    st.body_cache
                        .insert(art_id.clone(), body.clone(), "web", token);
                }
                if token != st.content_token || st.current_body_art != art_id {
                    return;
                }
            }
            if let Some(article) = state
                .borrow()
                .current_articles
                .iter()
                .find(|a| a.id == art_id)
                .cloned()
            {
                set_content(w, state, &article, &body);
            }
        }
        AbMsg::Prefetched {
            art_id,
            body,
            cacheable,
        } => {
            if let Some(body) = body
                && cacheable
            {
                let mut st = state.borrow_mut();
                st.cached_ids.insert(art_id.clone());
                let token = st.content_token;
                st.body_cache.insert(art_id, body, "web", token);
            }
        }
        AbMsg::Chapters {
            art_id,
            token,
            chapters,
        } => {
            {
                let mut st = state.borrow_mut();
                st.chapter_inflight.remove(&art_id);
                if !chapters.is_empty() {
                    st.chapter_cache.insert(art_id.clone(), chapters);
                }
                if token != st.content_token || st.current_body_art != art_id {
                    return;
                }
            }
            if let Some(article) = state
                .borrow()
                .current_articles
                .iter()
                .find(|a| a.id == art_id)
                .cloned()
            {
                let body = state.borrow().current_body_text.clone();
                set_content(w, state, &article, &body);
            }
        }
        AbMsg::Rich {
            art_id,
            token,
            html,
            url,
        } => {
            let st = state.borrow();
            if token != st.content_token || st.current_body_art != art_id {
                return;
            }
            drop(st);
            if let (Some(view), Some(html)) = (&w.web, html) {
                view.set_page(&html, &url);
            }
        }
        AbMsg::PlayResolved {
            article,
            url,
            queue,
            index,
        } => match url {
            Some(url) => {
                let headers = crate::playback_headers_for(&article, &url, &crate::db_path());
                crate::player::open_player_queue(&w.frame, *article, url, queue, index, headers);
            }
            None => {
                if !article.url.is_empty() {
                    crate::open_in_browser(&article.url);
                }
            }
        },
        AbMsg::RefreshDone => {
            refresh_views(w, state);
        }
    }
}
