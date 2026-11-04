//! BlindRSS (Rust) — main window over the existing Python database.
//! Slice 2: Refresh All (worker-thread fetch, main-thread store),
//! feed filtering via the tree, mark-read on open.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

mod article_window;
mod casting;
mod dialogs;
mod help;
mod hotkey;
mod keys;
mod media_backend;
mod msaa_names;
mod player;
mod settings;
mod startup;
mod update;

use blindrss_core::{Article, Db, Feed};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use wxdragon::event::webview_events::WebViewEvents;
use wxdragon::prelude::*;
use wxdragon::widgets::webview::{WebView, WebViewBackend};

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
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

/// True for the Program Files Windows installer distribution
/// (Python's `is_windows_installed_build`): Windows plus the
/// `.windows-installed` marker beside the executable. The Rust
/// binary is always a built executable, so `sys.frozen` has no
/// separate counterpart.
fn windows_installed_build() -> bool {
    #[cfg(target_os = "windows")]
    {
        app_dir().join(".windows-installed").is_file()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Resolve the data directory the way Python's ConfigManager does
/// (`core/config.py::_resolve_config_path`): installed Windows
/// builds always use user data; when config.json exists in both
/// locations the newer one wins; a single existing config wins;
/// otherwise the platform default (user data on installed Windows
/// builds and macOS, the app folder elsewhere). Installed Windows
/// builds first seed the user location from a legacy app-folder
/// config (Python's `_prepare_windows_installed_data`).
fn data_dir() -> PathBuf {
    let user_config = user_data_dir().join("config.json");
    let app_config = app_dir().join("config.json");
    if windows_installed_build() {
        blindrss_core::config::copy_file_if_missing(&app_config, &user_config);
    }
    let default_is_user = windows_installed_build() || cfg!(target_os = "macos");
    let resolved = blindrss_core::config::resolve_config_path(
        &user_config,
        &app_config,
        windows_installed_build(),
        default_is_user,
    );
    resolved
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(user_data_dir)
}

fn config_path() -> PathBuf {
    std::env::var("BLINDRSS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir().join("config.json"))
}

/// Map a stored `last_selected_feed` value (shared Python format,
/// Audit 8 G20) to the startup state: the Rust tree selection and
/// the read filter. Favorites has a real tree node (Audit 8
/// G06); Deleted has none (it is a dialog), so it restores as
/// the All view; the stored value itself is left untouched in
/// the config until the user selects a view.
fn restored_view_state(raw: &str) -> (Option<String>, u8) {
    let view = blindrss_core::config::decode_remembered_view(raw);
    let selected = match view.view_id.as_deref() {
        Some("deleted:all") => None,
        other => other.map(|s| s.to_string()),
    };
    (selected, view.read_filter)
}

/// The shared-format value to store for a tree selection (Audit 8
/// G20): the inverse of [`restored_view_state`]'s decode, via
/// core's Python-compatible encoder.
fn remembered_view_value(selected: Option<&str>, read_filter: u8) -> String {
    blindrss_core::config::encode_remembered_view(selected, read_filter)
}

/// Subscribe worker body (Audit 8 F01/D22): resolve, insert and
/// store a new feed on the calling (worker) thread, through its
/// own DB connection. Returns the created feed row.
fn subscribe_on_worker(url: &str, category: Option<&str>) -> Result<Feed, String> {
    let db = Db::open(&db_path()).map_err(|e| format!("Database: {e}"))?;
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    blindrss_core::refresh::subscribe_url(
        &db,
        url,
        category,
        cfg.get_str("delete_behavior", "deleted"),
        article_retention_cutoff().as_deref(),
    )
}

/// The retention boundary ingestion must not insert past (Python
/// issue #106): the `YYYY-MM-DD` cutoff for the configured
/// `article_retention`, or None when retention is unlimited.
fn article_retention_cutoff() -> Option<String> {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    blindrss_core::retention::retention_cutoff_date(blindrss_core::retention::retention_days(
        cfg.get_str("article_retention", "unlimited"),
    ))
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
/// Full-text arbitration (PARITY 7.6): when the active
/// provider is Miniflux and the article is one of its entries,
/// the server's fetch-content result competes with the local
/// cascade outcome; the more complete text wins. Runs on the
/// full-text worker thread.
/// Automatic translation inside the full-text worker
/// (Python's worker-side `_translate_rendered_text_if_enabled`,
/// Audit 8 O07): when a translation runtime config exists,
/// the extracted text is translated before it is cached or
/// shown; any failure keeps the original text, so a blank or
/// failing translation never replaces the article.
fn auto_translate_text(text: String) -> String {
    if text.trim().is_empty() {
        return text;
    }
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let Some(settings) = blindrss_core::translate::settings_from_config(&cfg) else {
        return text;
    };
    match blindrss_core::translate::translate_text(&cfg, &text, &settings.target_language) {
        Ok(translated) if !translated.trim().is_empty() => translated,
        _ => text,
    }
}

fn arbitrate_full_text(
    outcome: blindrss_core::extract::ExtractOutcome,
    article_id: &str,
) -> blindrss_core::extract::ExtractOutcome {
    use blindrss_core::extract::ExtractOutcome as EO;
    let Some(entry) = article_id
        .strip_prefix("miniflux-entry:")
        .and_then(|s| s.parse::<i64>().ok())
    else {
        return outcome;
    };
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    if blindrss_core::provider::active_name(&cfg) != "miniflux" {
        return outcome;
    }
    let (url, key) = (
        cfg.raw()
            .get("providers")
            .and_then(|p| p.get("miniflux"))
            .and_then(|s| s.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        cfg.raw()
            .get("providers")
            .and_then(|p| p.get("miniflux"))
            .and_then(|s| s.get("api_key"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    );
    if url.is_empty() || key.is_empty() {
        return outcome;
    }
    let Ok(html) =
        blindrss_core::providers::MinifluxClient::new(&url, &key).fetch_content_remote(entry)
    else {
        return outcome;
    };
    let remote_text =
        blindrss_core::extract::html_to_text_opts(&html, cfg.get_bool("show_image_alt", false));
    let local_len = match &outcome {
        EO::Article { text, .. } => text.chars().count(),
        _ => 0,
    };
    if remote_text.chars().count() > local_len {
        EO::Article {
            title: String::new(),
            text: remote_text,
            source: "miniflux",
        }
    } else {
        outcome
    }
}

/// Cast one resolved stream to one device, dispatching on
/// its protocol. Non-Cast devices receive the stream through
/// the local proxy (they cannot take resolver URLs or local
/// paths); Chromecast takes the URL directly.
/// The running AirPlay cast's stop flag (RAOP streams FROM
/// this app, unlike the other protocols, so it needs an
/// explicit stop).
static RAOP_STOP: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
> = std::sync::OnceLock::new();

fn raop_stop_registry()
-> &'static std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> {
    RAOP_STOP.get_or_init(|| std::sync::Mutex::new(None))
}

pub(crate) fn playback_headers_for(
    article: &Article,
    media_url: &str,
    path: &std::path::Path,
) -> Vec<(String, String)> {
    let channel = Db::open(path)
        .ok()
        .and_then(|db| db.feed_settings(&article.feed_id).ok())
        .unwrap_or_default();
    let cookies = blindrss_core::cookies::load_jar_beside(&config_path());
    blindrss_core::http_headers::playback_headers(
        &channel,
        media_url,
        &article.url,
        article.media_url.as_deref().unwrap_or(&article.url),
        &cookies,
    )
}

/// The play-cache step (youtube_play_via_download): when a
/// YouTube stream was resolved through yt-dlp and the cache
/// is enabled, download the audio once and play the file.
/// Any failure keeps the streamed URL.
fn maybe_cache_youtube(article: &Article, stream_url: &str, via_ytdlp: bool) -> String {
    if !via_ytdlp {
        return stream_url.to_string();
    }
    let is_youtube = article.url.contains("youtube.com") || article.url.contains("youtu.be");
    if !is_youtube {
        return stream_url.to_string();
    }
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    if !cfg.get_bool("youtube_play_via_download", false) {
        return stream_url.to_string();
    }
    let dir = blindrss_core::play_cache::cache_dir(
        cfg.get_str("youtube_play_cache_dir", ""),
        &data_dir(),
    );
    let max_mb = cfg.get_i64("youtube_play_cache_max_mb", 500);
    match blindrss_core::play_cache::fetch_youtube_audio(&article.url, &dir, max_mb) {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(_) => stream_url.to_string(),
    }
}

/// The video id of a recommendation article id
/// ("youtube-account:recommendations:<account>:<video_id>").
fn recommendation_video_id(article_id: &str) -> Option<&str> {
    article_id
        .strip_prefix(blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID)?
        .strip_prefix(':')?
        .split_once(':')
        .map(|(_, video)| video)
}

/// Read/favorite writes route to the recommendations table for
/// virtual-feed articles (their ids are not in `articles`).
/// Read state is SHARED with the local feed article for the
/// same video (Audit 8 D17, Python's v2.5.8 twin rule): marking
/// either copy marks the other. The twin rule is local-provider
/// only — hosted account article state is never merged.
pub(crate) fn mark_article_read(
    db: &blindrss_core::db::Db,
    article: &blindrss_core::db::Article,
    read: bool,
) {
    let is_rec = article.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID;
    if is_rec {
        let _ = db.set_recommendation_state(&article.id, Some(read), None);
    } else {
        let _ = db.mark_read(&article.id, read);
    }
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    if blindrss_core::provider::active_name(&cfg) != "local" {
        return;
    }
    if is_rec {
        if let Some(video_id) = recommendation_video_id(&article.id) {
            let _ = db.mark_read(&format!("yt:video:{video_id}"), read);
        }
    } else if let Some(video_id) = article.id.strip_prefix("yt:video:") {
        let token = cfg.get_str("youtube_account_refresh_token", "");
        let account = blindrss_core::youtube_account::account_key(token);
        if !account.is_empty() {
            let rec_id = format!(
                "{}:{account}:{video_id}",
                blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID
            );
            let _ = db.set_recommendation_state(&rec_id, Some(read), None);
        }
    }
}

/// Mark the currently selected article read and opened —
/// Python's two mark triggers are the reader gaining focus
/// (`on_content_focus`) and activation (`on_article_activate`);
/// list selection alone only previews (Audit 8 G01). Opened is
/// recorded unconditionally (it backs the Smart Folders
/// "opened" criterion); read only flips when the article was
/// unread. Returns the article when its read state changed,
/// so the caller can propagate to a hosted provider after
/// releasing the state borrow.
fn mark_selected_read_opened(st: &mut State, frame: &Frame) -> Option<Article> {
    let idx = st.selected_article?;
    let a = st.articles.get(idx)?.clone();
    if let Some(db) = st.db.as_ref() {
        let _ = db.mark_opened(&a.id);
    }
    if a.is_read {
        return None;
    }
    if let Some(db) = st.db.as_ref() {
        mark_article_read(db, &a, true);
    }
    if let Some(slot) = st.articles.get_mut(idx) {
        slot.is_read = true;
    }
    frame.set_status_text(&format!("Read: {}", a.title), 0);
    Some(a)
}

pub(crate) fn mark_article_favorite(
    db: &blindrss_core::db::Db,
    article: &blindrss_core::db::Article,
    favorite: bool,
) {
    if article.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
        let _ = db.set_recommendation_state(&article.id, None, Some(favorite));
    } else {
        let _ = db.set_favorite(&article.id, favorite);
    }
}

/// Build the GReader client for the ACTIVE hosted account
/// (theoldreader / bazqux / inoreader), or None when another
/// provider is active or its credentials are unset. Mirrors
/// the client construction in propagate_read_state.
fn active_greader_client(
    cfg: &blindrss_core::config::Config,
) -> Option<blindrss_core::providers::GReaderClient> {
    use blindrss_core::providers::{GReaderClient, GReaderService};
    let providers = cfg.raw().get("providers")?;
    let get = |obj: &serde_json::Value, key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match blindrss_core::provider::active_name(cfg).as_str() {
        "theoldreader" | "bazqux" => {
            let key = blindrss_core::provider::active_name(cfg);
            let service = if key == "bazqux" {
                GReaderService::BazQux
            } else {
                GReaderService::TheOldReader
            };
            let section = providers.get(&key)?;
            let (email, password) = (get(section, "email"), get(section, "password"));
            if email.is_empty() || password.is_empty() {
                return None;
            }
            Some(GReaderClient::new(service, &email, &password))
        }
        "inoreader" => {
            let section = providers.get("inoreader")?;
            let token = get(section, "token");
            if token.is_empty() {
                return None;
            }
            Some(GReaderClient::with_oauth(GReaderService::Inoreader, &token))
        }
        _ => None,
    }
}

/// Mirror a category rename/delete to the hosted server when a
/// hosted provider is active (Python's provider methods):
/// Miniflux renames/deletes its server category, the GReader
/// family renames/disables the label. A refusal surfaces as a
/// status note rather than a phantom local-only success.
fn mirror_hosted_category(
    tx: &std::sync::mpsc::Sender<RefreshMsg>,
    rename: Option<(String, String)>,
    delete: Option<String>,
) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        // GReader family: labels are the categories.
        if let Some(client) = active_greader_client(&cfg) {
            let (label, result) = if let Some((old, new)) = &rename {
                (old.clone(), client.rename_label_remote(old, new))
            } else if let Some(title) = &delete {
                (title.clone(), client.disable_label_remote(title))
            } else {
                return;
            };
            if let Err(e) = result {
                let _ = tx.send(RefreshMsg::StatusNote(format!(
                    "Category {label} sync failed: {e}"
                )));
            }
            return;
        }
        if blindrss_core::provider::active_name(&cfg) != "miniflux" {
            return;
        }
        let get = |key: &str| -> String {
            cfg.raw()
                .get("providers")
                .and_then(|p| p.get("miniflux"))
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (url, key) = (get("url"), get("api_key"));
        if url.is_empty() || key.is_empty() {
            return;
        }
        let client = blindrss_core::providers::MinifluxClient::new(&url, &key);
        let (label, result) = if let Some((old, new)) = &rename {
            (old.clone(), client.rename_category_remote(old, new))
        } else if let Some(title) = &delete {
            (title.clone(), client.delete_category_remote(title))
        } else {
            return;
        };
        match result {
            Ok(true) => {}
            Ok(false) => {
                let _ = tx.send(RefreshMsg::StatusNote(format!(
                    "Miniflux: category {label} was not changed on the server"
                )));
            }
            Err(e) => {
                let _ = tx.send(RefreshMsg::StatusNote(format!(
                    "Miniflux category sync failed: {e}"
                )));
            }
        }
    });
}

/// Mirror a category creation to Miniflux when it is the
/// active provider: Miniflux categories are flat, so only the
/// leaf of the local path is created (Python's add_category
/// ignores the parent the same way). The GReader family needs
/// no call — Python's add_category there is a no-op because
/// labels come into existence when a feed is assigned one
/// (see act_set_cat / push_feed_update).
fn mirror_hosted_category_create(tx: &std::sync::mpsc::Sender<RefreshMsg>, full_path: String) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        if blindrss_core::provider::active_name(&cfg) != "miniflux" {
            return;
        }
        let get = |key: &str| -> String {
            cfg.raw()
                .get("providers")
                .and_then(|p| p.get("miniflux"))
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (url, key) = (get("url"), get("api_key"));
        if url.is_empty() || key.is_empty() {
            return;
        }
        let leaf = blindrss_core::category_display_leaf(full_path.trim());
        if leaf.is_empty() {
            return;
        }
        if let Err(e) =
            blindrss_core::providers::MinifluxClient::new(&url, &key).create_category_remote(&leaf)
        {
            let _ = tx.send(RefreshMsg::StatusNote(format!(
                "Miniflux category sync failed: {e}"
            )));
        }
    });
}

/// Push a feed edit (properties dialog, Edit Feed, Set Feed
/// Category) through the active hosted provider on a worker:
/// provider.update_feed does the remote edit first and then
/// re-applies the cache writes the GUI already made, so the
/// re-apply is idempotent. A remote failure arrives as a
/// ProviderAction error and surfaces in the status bar.
/// `before` is the pre-edit snapshot — the GReader path needs
/// its category to remove the old label. Only changed fields
/// are passed (None = unchanged); a category of Some("") means
/// the category was cleared.
fn push_feed_update(
    tx: &std::sync::mpsc::Sender<RefreshMsg>,
    before: blindrss_core::Feed,
    title: Option<String>,
    url: Option<String>,
    category: Option<String>,
) {
    if title.is_none() && url.is_none() && category.is_none() {
        return;
    }
    let tx = tx.clone();
    std::thread::spawn(move || {
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        if blindrss_core::provider::active_name(&cfg) == "local" {
            return;
        }
        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
        let result = provider
            .update_feed(
                &before,
                title.as_deref(),
                url.as_deref(),
                category.as_deref(),
            )
            .map(|_| format!("Feed {} updated on the server", before.title));
        let _ = tx.send(RefreshMsg::ProviderAction(result));
    });
}

pub(crate) fn propagate_read_state(article: &blindrss_core::db::Article, read: bool) {
    // Recommendations are local-only state.
    if article.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
        return;
    }
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
        if article.feed_id.starts_with("miniflux:")
            && blindrss_core::provider::active_name(&cfg) == "miniflux"
        {
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
            // The article belongs to the ACTIVE account (its
            // cache is the open database); write only there.
            // Spraying every configured GReader service leaked
            // read state into the user's other accounts when
            // the same item id existed on both.
            match blindrss_core::provider::active_name(&cfg).as_str() {
                "theoldreader" | "bazqux" => {
                    let key = blindrss_core::provider::active_name(&cfg);
                    let service = if key == "bazqux" {
                        GReaderService::BazQux
                    } else {
                        GReaderService::TheOldReader
                    };
                    if let Some(section) = providers.get(&key) {
                        let (email, password) = (get(section, "email"), get(section, "password"));
                        if !email.is_empty() && !password.is_empty() {
                            let _ = GReaderClient::new(service, &email, &password)
                                .set_read_remote(&article.id, read);
                        }
                    }
                }
                "inoreader" => {
                    if let Some(section) = providers.get("inoreader") {
                        let token = get(section, "token");
                        if !token.is_empty() {
                            let _ = GReaderClient::with_oauth(GReaderService::Inoreader, &token)
                                .set_read_remote(&article.id, read);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

/// Favorite counterpart of propagate_read_state: push a
/// favorite toggle to the ACTIVE hosted provider only (the
/// Batch 39 isolation rule). GReader favorites are the
/// server's starred state; Miniflux favorites are bookmarks,
/// set to the requested state (set_bookmark_remote reads the
/// server state and toggles only when it differs). The local
/// cache write already happened in mark_article_favorite.
pub(crate) fn propagate_favorite_state(article: &blindrss_core::db::Article, favorite: bool) {
    // Recommendations are local-only state.
    if article.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
        return;
    }
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
        if article.feed_id.starts_with("miniflux:")
            && blindrss_core::provider::active_name(&cfg) == "miniflux"
        {
            if let Some(section) = providers.get("miniflux") {
                let (url, key) = (get(section, "url"), get(section, "api_key"));
                if let Some(entry) = article
                    .id
                    .strip_prefix("miniflux-entry:")
                    .and_then(|s| s.parse::<i64>().ok())
                    && !url.is_empty()
                    && !key.is_empty()
                {
                    let _ = blindrss_core::providers::MinifluxClient::new(&url, &key)
                        .set_bookmark_remote(entry, favorite);
                }
            }
            return;
        }
        if article.feed_id.starts_with("feed/")
            && let Some(client) = active_greader_client(&cfg)
        {
            let _ = client.set_starred_remote(&article.id, favorite);
        }
    });
}

/// Build an article stub from a play-queue entry (config
/// "play_queue" object). The entry's media_url is both the
/// playback target and, for page URLs, the yt-dlp input.
fn queue_entry_article(e: &serde_json::Value) -> Article {
    let media_url = e
        .get("media_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Article {
        id: e
            .get("article_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&media_url)
            .to_string(),
        feed_id: e
            .get("feed_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        title: e
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&media_url)
            .to_string(),
        url: media_url.clone(),
        content: None,
        description: None,
        date: None,
        author: None,
        is_read: true,
        is_favorite: false,
        media_url: Some(media_url),
        media_type: e
            .get("media_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
    }
}

/// Apply the fetch-identity config to the shared HTTP agent
/// and to the environment yt-dlp reads (cookies file + UA).
/// Called at startup and after Settings changes.
fn apply_http_config() {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    blindrss_core::http::init_from_config(&cfg);
    blindrss_core::forum_sources::configure(&cfg, &config_path());
    blindrss_core::feeds::configure_cookies(&config_path());
    blindrss_core::dependency::init_tool_paths_from_config(&cfg);
    blindrss_core::dependency::set_managed_ytdlp_dir(&data_dir());
    blindrss_core::source_feeds::configure(&config_path());
    // SAFETY: called on the UI thread before workers that read
    // these variables are spawned for a new action; this is the
    // same trade-off the process env always carries.
    unsafe {
        std::env::set_var(
            "BLINDRSS_YTDLP_COOKIES",
            cfg.get_str("ytdlp_cookies_file", ""),
        );
        std::env::set_var("BLINDRSS_USER_AGENT", blindrss_core::http::user_agent());
        // The resolved UI language for core consumers that rank by
        // it (YouTube subtitle track selection in ytfull).
        std::env::set_var("BLINDRSS_UI_LANGUAGE", ui_language());
        // Same for the managed ffmpeg/ffprobe install: point the
        // finders at its bin dir when the user has not pinned
        // their own binaries.
        if let Some(bin) = blindrss_core::dependency::managed_ffmpeg_bin_dir(&data_dir()) {
            let exe = |tool: &str| {
                if cfg!(windows) {
                    bin.join(format!("{tool}.exe"))
                } else {
                    bin.join(tool)
                }
            };
            if std::env::var_os("BLINDRSS_FFMPEG").is_none() {
                std::env::set_var("BLINDRSS_FFMPEG", exe("ffmpeg"));
            }
            if std::env::var_os("BLINDRSS_FFPROBE").is_none() {
                std::env::set_var("BLINDRSS_FFPROBE", exe("ffprobe"));
            }
        }
    }
}

/// Play a sound file with the platform's command-line player
/// (no audio dependency in the tree): PowerShell SoundPlayer
/// on Windows, afplay on macOS, paplay/aplay on Linux.
fn play_sound_file(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let p = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "(New-Object System.Media.SoundPlayer '{}').PlaySync()",
            p.replace('\'', "''")
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("afplay").arg(&p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::process::Command::new("paplay")
            .arg(&p)
            .spawn()
            .is_err()
        {
            let _ = std::process::Command::new("aplay").arg(&p).spawn();
        }
    }
}

/// Resolve a configured sound path ("sounds/refresh_complete.
/// wav" in Python's config): beside the executable first (the
/// release zips carry a sounds/ folder), then the data dir,
/// then as given.
fn resolve_sound(
    cfg: &blindrss_core::config::Config,
    key: &str,
    default: &str,
) -> std::path::PathBuf {
    let raw = cfg.get_str(key, default);
    let rel = std::path::Path::new(raw);
    if rel.is_absolute() {
        return rel.to_path_buf();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(rel);
        if candidate.exists() {
            return candidate;
        }
    }
    let in_data = data_dir().join(rel);
    if in_data.exists() {
        return in_data;
    }
    rel.to_path_buf()
}

/// Startup maintenance on a worker: automatic backup when due
/// (24h, keep newest 7) and download-folder retention pruning.
fn startup_maintenance() {
    std::thread::spawn(move || {
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        if cfg.get_bool("auto_backup_enabled", false) {
            let last = cfg
                .raw()
                .get("auto_backup_last")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            if blindrss_core::maintenance::auto_backup_due(last, now) {
                let raw_dir = cfg.get_str("auto_backup_dir", "").trim().to_string();
                let dir = if raw_dir.is_empty() {
                    data_dir().join("backups")
                } else {
                    PathBuf::from(raw_dir)
                };
                // Python's run_auto_backup: timestamped
                // `BlindRSS-auto-backup-YYYY-MM-DD-HHMMSS.zip` name
                // plus rotation over prefix+`.zip` names only, so
                // Rust and Python backups in one folder rotate
                // together and nothing else is ever deleted.
                if blindrss_core::maintenance::run_auto_backup(
                    &dir,
                    &data_dir(),
                    &config_path(),
                    &db_path(),
                    blindrss_core::maintenance::AUTO_BACKUP_KEEP,
                )
                .is_ok()
                {
                    let mut cfg2 =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    cfg2.set_path("auto_backup_last", serde_json::json!(now));
                    let _ = cfg2.save(&config_path());
                }
            }
        }
        // Download retention: delete files older than the policy.
        if let Some(days) =
            blindrss_core::retention::retention_days(cfg.get_str("download_retention", "unlimited"))
        {
            let dir = cfg
                .raw()
                .get("download_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            if let Some(dir) = dir {
                let cutoff = std::time::SystemTime::now()
                    - std::time::Duration::from_secs((days.max(0) as u64) * 86_400);
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for entry in rd.flatten() {
                        let path = entry.path();
                        let old = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .map(|t| t < cutoff)
                            .unwrap_or(false);
                        if old && path.is_file() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
        }
    });
}

/// Side effects at the end of a refresh (Python's Sounds +
/// Notifications pages): completion/error sounds and desktop
/// notifications for feeds with new articles.
fn refresh_side_effects(tray: &TaskBarIcon, new_by_feed: &[(String, String, usize)], failed: bool) {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    if cfg.get_bool("sounds_enabled", true) {
        let (key, default) = if failed {
            ("sound_refresh_error", "sounds/refresh_error.wav")
        } else {
            ("sound_refresh_complete", "sounds/refresh_complete.wav")
        };
        play_sound_file(&resolve_sound(&cfg, key, default));
    }
    if !cfg.get_bool("windows_notifications_enabled", false) {
        return;
    }
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
    let include_name = cfg.get_bool("windows_notifications_include_feed_name", true);
    let items: Vec<&(String, String, usize)> = new_by_feed
        .iter()
        .filter(|(id, _, _)| !excluded.iter().any(|x| x == id))
        .collect();
    if items.is_empty() {
        return;
    }
    let cap = cfg.get_i64("windows_notifications_max_per_refresh", 0);
    let shown = if cap <= 0 {
        items.len()
    } else {
        (cap as usize).min(items.len())
    };
    for (_, title, n) in &items[..shown] {
        let body = if include_name {
            format!("{n} new article(s) in {title}")
        } else {
            format!("{n} new article(s)")
        };
        notify_desktop(tray, "BlindRSS", &body);
    }
    if shown < items.len() && cfg.get_bool("windows_notifications_show_summary_when_capped", true) {
        let total: usize = items.iter().map(|(_, _, n)| n).sum();
        notify_desktop(
            tray,
            "BlindRSS",
            &format!("{total} new article(s) in {} feed(s)", items.len()),
        );
    }
}

/// One desktop notification: the tray balloon on Windows
/// (wxTaskBarIcon::ShowBalloon), notify-send on Linux,
/// osascript on macOS.
fn notify_desktop(tray: &TaskBarIcon, title: &str, body: &str) {
    #[cfg(target_os = "windows")]
    {
        tray.show_balloon(title, body, 5000, 0, None);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = tray;
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('\"', "'"),
            title.replace('\"', "'")
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = tray;
        let _ = std::process::Command::new("notify-send")
            .args([title, body])
            .spawn();
    }
}

/// Modal text prompt; None when cancelled.
/// Speak/braille a configured announcement for an event
/// (PARITY 7.4). Config is re-read per event so Settings changes
/// apply live, as in Python.
fn announce(event: &str, message: &str) {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let _ = blindrss_core::announce::Announcements::from_config(&cfg).announce(event, message);
}

pub(crate) fn open_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Open an article link using the configured method (Python
/// `_open_article_url`, issue #31): with the "custom" method, run
/// the user's command template (%1 = URL); on any failure, warn
/// and fall back to the default browser so the user still gets
/// their article.
/// The rich reader's document skeleton: the WebView owns the
/// document (Python's design), the cleaned fragment drops into the
/// body, and the fragment's `<article lang="...">` carries the
/// content language for assistive tech.
fn open_reader_link(reader: &TextCtrl, frame: &Frame) -> bool {
    let Some((x, y)) = reader.position_to_xy(reader.get_insertion_point()) else {
        return false;
    };
    let line = reader.get_line_text(y);
    let mut units = 0;
    let cursor = line
        .char_indices()
        .find_map(|(byte, ch)| {
            if units >= x.max(0) as usize {
                Some(byte)
            } else {
                units += if cfg!(windows) { ch.len_utf16() } else { 1 };
                None
            }
        })
        .unwrap_or(line.len());
    if let Some(url) = blindrss_core::article_html::inline_link_at(&line, cursor) {
        open_article_link(frame, &url);
        true
    } else {
        false
    }
}

pub(crate) fn rich_document(fragment: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <style>body {{ font-family: sans-serif; line-height: 1.5; margin: 1em; }} \
         img, iframe, video {{ max-width: 100%; }}</style></head>\
         <body>{fragment}</body></html>"
    )
}

/// Python rich_view_links.external_url: a plain HTTP(S) URL safe to
/// hand to the OS (host present, no credentials, no whitespace,
/// control characters, or backslashes).
fn rich_external_url(href: &str) -> Option<String> {
    if href.is_empty() || href.contains('\\') {
        return None;
    }
    if href
        .chars()
        .any(|c| c.is_whitespace() || (c as u32) < 32 || (127..=159).contains(&(c as u32)))
    {
        return None;
    }
    let lower = href.to_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(href.to_string())
}

/// True for social/player embed URLs, which only ever appear as
/// iframe sources and must load inside the WebView (Python issue
/// #102). YouTube/Vimeo iframe sources are normalized to watch pages
/// by the cleaner, so they are absent here on purpose: if one ever
/// navigates, the browser is the right home for it.
fn rich_embed_url(url: &str) -> bool {
    let low = url.to_lowercase();
    low.contains("platform.twitter.com/embed/")
        || low.contains("embed.bsky.app/embed/")
        || low.contains("facebook.com/plugins/")
        || low.contains("tiktok.com/embed/")
        || (low.contains("instagram.com/") && low.contains("/embed"))
        || ((low.contains("threads.net/") || low.contains("threads.com/"))
            && low.contains("/embed"))
}

/// Fetch + render the full rich article on a worker (the rich
/// counterpart of the full-text fetch).
fn spawn_rich_fetch(tx: &Sender<RefreshMsg>, article: &blindrss_core::db::Article) {
    let tx = tx.clone();
    let id = article.id.clone();
    let url = article.url.clone();
    let fallback = article
        .content
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| article.description.clone())
        .unwrap_or_default();
    let title = article.title.clone();
    let author = article.author.clone().unwrap_or_default();
    let date = article
        .date
        .clone()
        .unwrap_or_default()
        .split('T')
        .next()
        .unwrap_or("")
        .to_string();
    std::thread::spawn(move || {
        let jar = blindrss_core::cookies::load_jar_beside(&config_path());
        let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
        let frag = blindrss_core::article_html::render_rich_article(
            &url,
            &fallback,
            &title,
            &author,
            &date,
            &header,
            "",
            "",
            &ui_language(),
        );
        let _ = tx.send(RefreshMsg::RichHtml(id, frag));
    });
}

fn open_article_link(frame: &Frame, url: &str) {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    if cfg
        .get_str("article_open_method", "default")
        .eq_ignore_ascii_case("custom")
    {
        let template = cfg.get_str("article_open_command", "").to_string();
        if !template.trim().is_empty() {
            let failure: Option<String> =
                match blindrss_core::opencmd::build_open_command(&template, url) {
                    Ok(argv) => match std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .spawn()
                    {
                        Ok(_) => None,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            Some(format!("Command not found: {}", argv[0]))
                        }
                        Err(e) => Some(e.to_string()),
                    },
                    Err(e) => Some(e),
                };
            match failure {
                None => return,
                Some(err) => {
                    let dlg = MessageDialog::builder(
                        frame,
                        &format!(
                            "Could not open the article with your custom command:\n\n{err}\n\nOpening in the default browser instead. You can change this in Settings > Article opening method."
                        ),
                        "Custom command failed",
                    )
                    .build();
                    dlg.show_modal();
                }
            }
        }
    }
    open_in_browser(url);
}

/// Resolve image-alt display for a feed (Python
/// `_show_images_for_feed`): the per-feed override wins, else
/// the global `show_image_alt` setting (default off).
fn show_image_alt_for(cfg: &blindrss_core::config::Config, db: Option<&Db>, feed_id: &str) -> bool {
    if let Some(db) = db
        && let Ok(Some(override_)) = db.feed_show_images(feed_id)
    {
        return override_;
    }
    cfg.get_bool("show_image_alt", false)
}

/// Re-render the selected article in place after a display
/// setting changed (Audit 8 G16; Python's
/// `on_set_feed_images` re-runs `_update_content_view`): the
/// article's cached renderings are invalidated, the reader
/// generation moves on so in-flight results retire, and the
/// feed body is shown again under the new setting — plain and
/// rich outputs alike — without moving the selection.
#[allow(clippy::too_many_arguments)]
fn rerender_current_article(
    state: &Rc<RefCell<State>>,
    frame: &Frame,
    reader: &TextCtrl,
    rich_view: &Rc<Option<WebView>>,
    rich_loading: &Rc<std::cell::Cell<bool>>,
    tx: &Sender<RefreshMsg>,
) {
    let mut st = state.borrow_mut();
    let Some(idx) = st.selected_article else {
        return;
    };
    let Some(mut a) = st.articles.get(idx).cloned() else {
        return;
    };
    ensure_content(st.db.as_ref(), &mut a);
    if let Some(row) = st.articles.get_mut(idx) {
        row.content = a.content.clone();
    }
    st.reader_generation = st.reader_generation.wrapping_add(1);
    st.pending_fulltext = None;
    st.auto_load = None;
    st.reader_cache.remove_prefix(&format!("{}:", a.id));
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let keep_alt = show_image_alt_for(&cfg, st.db.as_ref(), &a.feed_id);
    let raw_body = a
        .content
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| a.description.clone())
        .unwrap_or_else(|| "(no stored text)".to_string());
    let body = blindrss_core::extract::html_to_text_opts(&raw_body, keep_alt);
    reader.set_value(&format!("{}\n\n{}", a.title, body));
    if cfg.get_bool("full_text_rich_view", false)
        && let Some(wv) = rich_view.as_ref()
    {
        {
            let frag = blindrss_core::article_html::rich_article_from_feed(
                &a.url,
                &raw_body,
                &a.title,
                a.author.as_deref().unwrap_or(""),
                a.date
                    .as_deref()
                    .unwrap_or("")
                    .split('T')
                    .next()
                    .unwrap_or(""),
                "",
                "",
                &ui_language(),
            );
            match frag {
                Some(frag) => {
                    rich_loading.set(true);
                    wv.set_page(&rich_document(&frag), &a.url);
                    wv.show(true);
                    reader.show(false);
                    frame.layout();
                    spawn_rich_fetch(tx, &a);
                }
                None => {
                    wv.show(false);
                    reader.show(true);
                }
            }
            return;
        }
    }
    if let Some(wv) = rich_view.as_ref() {
        wv.show(false);
    }
    reader.show(true);
}

/// The config's notification-excluded feed ids (Python
/// `_get_notification_excluded_feed_ids`).
fn notification_excluded_ids(cfg: &blindrss_core::config::Config) -> Vec<String> {
    cfg.raw()
        .get("windows_notifications_excluded_feeds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Add or remove a feed from the notification exclusion list
/// (Python `_set_notification_excluded_feed_ids`; stored sorted).
fn set_notification_excluded(feed_id: &str, excluded: bool) {
    let mut cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let mut ids = notification_excluded_ids(&cfg);
    ids.retain(|id| id != feed_id);
    if excluded {
        ids.push(feed_id.to_string());
    }
    ids.sort();
    cfg.set_path(
        "windows_notifications_excluded_feeds",
        serde_json::json!(ids),
    );
    let _ = cfg.save(&config_path());
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
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    blindrss_core::provider::active_db_path(&cfg, &base_db_path())
}

fn base_db_path() -> PathBuf {
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

/// The translation variant identity of a config (Python's
/// `_translation_fulltext_cache_suffix`): empty when no
/// translation runtime config exists, so untranslated and
/// translated renderings never share a cache entry (Audit 8
/// O07/O08).
fn translation_variant(cfg: &blindrss_core::config::Config) -> String {
    blindrss_core::translate::settings_from_config(cfg)
        .map(|s| s.cache_suffix())
        .unwrap_or_default()
}

fn reader_cache_key(article: &Article, keep_alt: bool, tr_variant: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (
        &article.id,
        &article.url,
        &article.content,
        &article.description,
        keep_alt,
        ui_language(),
        tr_variant,
    )
        .hash(&mut hash);
    // The article id prefixes the key so per-article
    // invalidation (image-alt changes, G16) can find every
    // variant of one article's entries.
    format!("{}:{:016x}", article.id, hash.finish())
}

/// Whether selecting an article starts the automatic
/// full-text load (Python `on_article_selected`): translation
/// is on (the pane must end up translated without a focus
/// action), or the platform is macOS (VoiceOver never fires
/// the reader focus event), or the article is a forum thread
/// or YouTube video whose feed body is only a stub (Audit 8
/// G02).
fn auto_fulltext_on_select(
    translation_ready: bool,
    platform_macos: bool,
    forum_thread: bool,
    youtube_video: bool,
) -> bool {
    translation_ready || platform_macos || forum_thread || youtube_video
}

/// Queue the automatic full-text load for the drain timer:
/// immediate when forced (reader focus), debounced 350 ms
/// otherwise (Python `_fulltext_debounce_ms`), and tied to
/// the current reader generation so a selection change
/// cancels it.
fn schedule_auto_fulltext(st: &mut State, force: bool) {
    let delay = if force {
        std::time::Duration::ZERO
    } else {
        std::time::Duration::from_millis(350)
    };
    st.auto_load = Some((st.reader_generation, std::time::Instant::now() + delay));
}

struct State {
    db: Option<Db>,
    store_path: PathBuf,
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
    refresh_paused_until: Option<std::time::Instant>,
    /// Article view filters (Python's Ctrl+1..6): read filter
    /// 0 = all, 1 = unread only, 2 = read only; media filter
    /// 0 = all, 1 = with media, 2 = without media.
    read_filter: u8,
    media_filter: u8,
    /// Article sort (config article_sort_by / article_sort_ascending).
    sort_by: String,
    sort_ascending: bool,
    /// Article-list paging (Audit 7 Finding A): total rows in
    /// the current view when the view pages, -1 when it does
    /// not. While `articles.len() < view_total` the list carries
    /// one extra trailing row, Python's "Load more items
    /// (Enter)" placeholder, at index `articles.len()`.
    view_total: i64,
    /// Active in-view search (Audit 8 G04): whitespace-split AND
    /// terms over the CURRENT view, empty = no search. Python
    /// keeps the filter applied across refills and view switches
    /// until the user clears it, so it lives in state rather
    /// than being a one-shot list replacement.
    search_terms: Vec<String>,
    /// Search mode at the time the search was run (config
    /// `search_mode`): true = titles only.
    search_title_only: bool,
    /// Tree context menu's Mark All Read target (Audit 8 G05):
    /// the (view, prompt) the context builder stashed for the
    /// node the menu was opened on, consumed by the scoped
    /// mark-all handler. The main command never reads this —
    /// its scope is global by construction.
    ctx_mark: Option<(Option<String>, String)>,
    /// Last find-in-article query (Ctrl+F / F3 repeat it).
    find_query: String,
    /// True while the search text box has keyboard focus; the
    /// Backspace toggle-read shortcut must not eat its editing.
    search_focused: bool,
    /// Category tree expansion memory (Python issue #33): the
    /// user's explicit expand/collapse choices, re-applied across
    /// rebuilds; category_tree_default_expanded covers the rest.
    expanded_cats: std::collections::HashSet<String>,
    collapsed_cats: std::collections::HashSet<String>,
    /// Signature of the article-column layout currently built
    /// in the list; a refill rebuilds columns only when this
    /// changes (clear_all + re-insert per refill was a freeze
    /// source — Python only rebuilds on layout changes too).
    cols_sig: String,
    /// A tree selection asked for a refill; the drain timer
    /// runs it once for the node the user lands on instead of
    /// refilling for every node arrowed past.
    refill_pending: bool,
    /// Per-feed new-article counts for the running refresh
    /// (sounds/notifications at AllDone consume them).
    refresh_new: Vec<(String, String, usize)>,
    /// Any feed failed in the running refresh (error sound).
    refresh_failed: bool,
    /// Article stub for an in-flight queue playback resolve (the
    /// article may not be in the current list).
    pending_play: Option<Article>,
    /// Queue context for the in-flight queue playback: the
    /// entry articles and the index being started.
    pending_queue: Option<(Vec<Article>, usize)>,
    reader_cache: blindrss_core::reader_cache::ReaderCache,
    reader_generation: u64,
    pending_fulltext: Option<(u64, String)>,
    /// A scheduled automatic full-text load (Audit 8 G02):
    /// (reader generation, when it comes due). The drain
    /// timer fires it through the Read Full Text action once
    /// due; a generation change cancels it.
    auto_load: Option<(u64, std::time::Instant)>,
}

/// A resolved playback stream: URL, optional yt-dlp info and request headers.
type MediaResolution = (
    String,
    Option<blindrss_core::media::MediaInfo>,
    Vec<(String, String)>,
);

enum RefreshMsg {
    /// One feed's worker refresh finished: the committed store
    /// outcome (Audit 8 D22 — the drain never stores).
    FeedDone(PathBuf, Feed, Result<blindrss_core::StoreOutcome, String>),
    AllDone(PathBuf),
    /// Result of adding a feed by URL: the created feed row (the
    /// worker resolved, inserted and stored — Audit 8 F01/D22).
    Added(String, Result<Feed, String>, Option<String>),
    /// Result of a hosted-provider sync: (provider name, (feeds, new)).
    ProviderDone(String, Result<(usize, usize), String>),
    /// Fetched article page HTML for full-text extraction.
    FullText(
        blindrss_core::reader_cache::ReaderRequest,
        blindrss_core::extract::ExtractOutcome,
        Option<String>,
    ),
    /// Rich (WebView) full article: (article id, fragment if any).
    RichHtml(String, Option<String>),
    /// yt-dlp resolution for playback: (stream URL, info if fetched).
    MediaReady(String, Result<MediaResolution, String>),
    /// A finished download: description of what landed where.
    DownloadDone(Article, Result<PathBuf, String>),
    /// A finished manual backup (worker): the saved path or error.
    BackupDone(Result<PathBuf, String>),
    /// Detect Audio finished for an article: the detected
    /// (media URL, media type), or None when no audio was found
    /// on the article's page (Python's on_detect_audio).
    AudioDetected(String, Option<(String, Option<String>)>),
    /// YouTube full text for an article.
    YoutubeText(
        blindrss_core::reader_cache::ReaderRequest,
        Result<String, String>,
    ),
    /// Archive rescan for a feed: (feed title, new count).
    ArchiveDone(String, Result<usize, String>),
    /// A finished cast attempt.
    CastDone(Result<String, String>),
    /// Cast discovery finished: the devices found plus the
    /// article and its resolved stream URL, for the picker.
    CastDevices(Vec<blindrss_core::cast::CastDevice>, Article, String),
    /// A finished active-provider action (hosted refresh,
    /// subscribe, unsubscribe): a human summary or an error.
    ProviderAction(Result<String, String>),
    /// A finished article translation: the request identity
    /// it was started under (article, generation, variant —
    /// Audit 8 O08) plus the result.
    Translated(
        blindrss_core::reader_cache::TranslationRequest,
        Result<String, String>,
    ),
    /// A second copy asked this one to come to the front.
    ActivateWindow,
    /// The Windows global show/hide hotkey fired.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    ToggleWindow,
    /// Background watcher (cookie auto-import, translation
    /// updates) reporting through the status bar.
    StatusNote(String),
    /// Startup dependency check found missing tools (names).
    DepsMissing(Vec<String>),
    /// Managed yt-dlp download finished.
    YtdlpUpdated(Result<std::path::PathBuf, String>),
    /// Automatic ffmpeg/ffprobe install finished (bin dir).
    FfmpegInstalled(Result<std::path::PathBuf, String>),
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
const M_STOP_CAST: i32 = 2207;
const M_FEED_NOTIF: i32 = 2208;
const M_IMGALT_DEFAULT: i32 = 2209;
const M_IMGALT_ALWAYS: i32 = 2210;
const M_IMGALT_NEVER: i32 = 2211;
const M_DEL_CAT_FEEDS: i32 = 2212;
const M_VIEW_DESC: i32 = 2213;
const M_TOGGLE_SEARCH: i32 = 2214;
const M_SEARCH: i32 = 2130;
const M_HISTORY: i32 = 2131;
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
const M_EDIT_SF: i32 = 2190;
const M_FILTER_RULES: i32 = 2191;
const M_FEED_PROPS: i32 = 2192;
const M_OPEN_ARTICLE: i32 = 2193;
const M_OPEN_MEDIA: i32 = 2194;
const M_VIDEO_SEARCH: i32 = 2195;
const M_PERSISTENT: i32 = 2205;
const M_ADD_SHORTCUTS: i32 = 2246;
const M_IMPORT_COOKIES: i32 = 2206;
const M_ABOUT: i32 = 2196;
const M_ARTICLE_WINDOW: i32 = 2197;
const M_PODCAST_ARCHIVE: i32 = 2198;
const M_QUEUE_TOGGLE: i32 = 2199;
const M_QUEUE_OPEN: i32 = 2200;
const M_QUEUE_NEXT: i32 = 2201;
const M_QUEUE_PREV: i32 = 2202;
const M_FEED_SEARCH: i32 = 2203;
const M_TRANSLATE: i32 = 2204;
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
const M_COPY_TEXT: i32 = 2215;
const M_SORT_DESC: i32 = 2216;
const M_SORT_STATUS: i32 = 2217;
const M_RICH_VIEW: i32 = 2218;
const M_OPEN_BROWSER: i32 = 2219;
const M_DOWNLOAD_AS: i32 = 2220;
const M_EDIT_SELECTED: i32 = 2221;
// Batch 41a (Audit 6 G1): ids added for the Python-shaped
// menus. Player transport ids dispatch through
// player::dispatch_command; the seek/volume command names
// still need their arms in player.rs (see PARITY Batch 41a).
const M_MARK_READ_SEL: i32 = 2223;
const M_MARK_UNREAD_SEL: i32 = 2224;
const M_QUEUE_ADD_SEL: i32 = 2225;
const M_QUEUE_REMOVE_SEL: i32 = 2226;
const M_DETECT_AUDIO: i32 = 2227;
const M_COPY_IMAGE: i32 = 2228;
const M_ACTIVATE: i32 = 2229;
const M_PLAYER_SHOW_HIDE: i32 = 2230;
const M_PLAYER_PLAY_PAUSE: i32 = 2231;
const M_PLAYER_STOP: i32 = 2232;
const M_PLAYER_REWIND: i32 = 2233;
const M_PLAYER_FF: i32 = 2234;
const M_PLAYER_VOL_UP: i32 = 2235;
const M_PLAYER_VOL_DOWN: i32 = 2236;
const M_SPEED_UP: i32 = 2237;
const M_SPEED_DOWN: i32 = 2238;
const M_SPEED_RESET: i32 = 2239;
const M_PLAYER_CHAPTERS: i32 = 2240;
const M_YOUTUBE_ACCOUNT: i32 = 2241;
const M_ANNOUNCE_VERSION: i32 = 2242;
const M_RESET_TITLE: i32 = 2243;
const M_EDIT_CAT: i32 = 2244;
const M_EQUALIZER: i32 = 2245;
const M_MARKALL_CTX: i32 = 2247;
/// Chapter Links submenu slots (Batch 41a): the article list
/// context menu fills `chapter_link_urls` with the selected
/// article's chapter hrefs and appends up to this many items.
const M_CHAPTER_LINK_BASE: i32 = 2900;
const M_CHAPTER_LINK_SLOTS: i32 = 16;

/// Route remappable commands through the same actions as native menus.
fn shortcut_menu_id(command: &str) -> Option<i32> {
    Some(match command {
        "feeds.add" => M_ADD_FEED,
        "feeds.remove" => M_REMOVE_FEED,
        "feeds.refresh_all" => M_REFRESH,
        "feeds.stop_refresh" => M_STOP_REFRESH,
        "feeds.refresh_selected" => M_REFRESH_FEED,
        "feeds.edit_selected" => M_EDIT_SELECTED,
        "feeds.mark_all_read" => M_MARKALL,
        "feeds.view_errors" => M_FEED_ERRORS,
        "feeds.copy_url" => M_COPY_FEED_URL,
        "feeds.add_category" => M_ADD_CAT,
        "feeds.remove_category" => M_DEL_CAT,
        "feeds.import_opml" => M_IMPORT_OPML,
        "feeds.import_youtube_takeout" => M_TAKEOUT,
        "feeds.export_opml" => M_EXPORT_OPML,
        "feeds.find_podcast" | "feeds.detect_page" => M_FEED_SEARCH,
        "feeds.podcast_archive" => M_PODCAST_ARCHIVE,
        "feeds.video_search" => M_VIDEO_SEARCH,
        "media.open_url" => M_OPEN_MEDIA,
        "article.open_url" => M_OPEN_ARTICLE,
        "article.open_browser" => M_OPEN_BROWSER,
        "article.copy_link" => M_COPY_URL,
        "article.copy_media_link" => M_COPY_MEDIA,
        "article.copy_text" => M_COPY_TEXT,
        "article.toggle_read" => M_TOGGLE_READ,
        "article.toggle_favorite" => M_FAVORITE,
        "article.delete" => M_DELETE_ARTICLE,
        "article.download" => M_DOWNLOAD,
        "article.download_as" => M_DOWNLOAD_AS,
        "article.toggle_queue" => M_QUEUE_TOGGLE,
        "article.view_description" => M_VIEW_DESC,
        "view.toggle_search" => M_TOGGLE_SEARCH,
        "view.rich_view" => M_RICH_VIEW,
        "filter.read_all" => M_FILTER_ALL,
        "filter.read_unread" => M_FILTER_UNREAD,
        "filter.read_read" => M_FILTER_READ,
        "filter.media_all" => M_MEDIA_ALL,
        "filter.media_with" => M_MEDIA_WITH,
        "filter.media_without" => M_MEDIA_WITHOUT,
        "sort.date" => M_SORT_DATE,
        "sort.name" => M_SORT_NAME,
        "sort.author" => M_SORT_AUTHOR,
        "sort.description" => M_SORT_DESC,
        "sort.feed" => M_SORT_FEED,
        "sort.status" => M_SORT_STATUS,
        "sort.ascending" => M_SORT_DIR,
        "queue.open" => M_QUEUE_OPEN,
        "queue.next" => M_QUEUE_NEXT,
        "queue.prev" => M_QUEUE_PREV,
        "tools.filter_rules" => M_FILTER_RULES,
        "tools.backup_data" => M_BACKUP,
        "tools.restore_data" => M_RESTORE,
        "tools.persistent_search" => M_PERSISTENT,
        "tools.keyboard_shortcuts" => M_KEYS,
        "tools.settings" => M_SETTINGS,
        "tools.check_updates" => M_UPDATE,
        "tools.import_site_cookies" => M_IMPORT_COOKIES,
        "tools.youtube_account" => M_YOUTUBE_ACCOUNT,
        "player.equalizer" => M_EQUALIZER,
        "help.user_guide" => M_HELP,
        _ => return None,
    })
}

fn selected_article_indices(list: &ListCtrl, state: &State) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut index = list.get_first_selected_item();
    while index >= 0 {
        if (index as usize) < state.articles.len() {
            indices.push(index as usize);
        }
        index = list.get_next_item(index as i64, ListNextItemFlag::All, ListItemState::Selected);
    }
    if indices.is_empty() {
        indices.extend(state.selected_article.filter(|i| *i < state.articles.len()));
    }
    indices
}

/// Deleted Articles dialog policy (Audit 8 G07): after a
/// restore/purge pass, only the rows whose operation SUCCEEDED
/// leave the list. A failed row stays visible — removing it
/// anyway (the old dialog's behavior) told the user a restore
/// happened when nothing was restored. `succeeded` carries
/// (feed_id, article_id) pairs, the tombstone identity.
fn retain_failed_deleted_rows(rows: Vec<Article>, succeeded: &[(String, String)]) -> Vec<Article> {
    rows.into_iter()
        .filter(|a| {
            !succeeded
                .iter()
                .any(|(feed_id, id)| feed_id == &a.feed_id && id == &a.id)
        })
        .collect()
}

/// Display title for a Deleted Articles row (Python falls back
/// to the URL, then a placeholder, for identity-only tombstones
/// with no snapshot title).
fn deleted_display_title(a: &Article) -> String {
    if !a.title.trim().is_empty() {
        a.title.clone()
    } else if !a.url.trim().is_empty() {
        a.url.clone()
    } else {
        "(deleted article)".to_string()
    }
}

fn has_direct_media_link(article: &Article) -> bool {
    article.media_url.as_deref().is_some_and(|u| {
        !u.trim().is_empty() && u != article.url && !blindrss_core::media::is_ytdlp_supported(u)
    })
}

/// First `<img src>` in article HTML (Python's
/// utils.first_image_url, for Copy Image Link).
fn first_image_url(content: Option<&str>) -> Option<String> {
    let html = content?;
    let lower = html.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find("<img") {
        let tag_start = search + pos;
        let tag_end = lower[tag_start..]
            .find('>')
            .map(|e| tag_start + e)
            .unwrap_or(lower.len());
        let tag = &html[tag_start..tag_end];
        let tag_lower = &lower[tag_start..tag_end];
        if let Some(sp) = tag_lower.find("src=") {
            let rest = tag[sp + 4..].trim_start();
            let value = if let Some(stripped) = rest.strip_prefix('"') {
                stripped.split('"').next().unwrap_or("")
            } else if let Some(stripped) = rest.strip_prefix('\'') {
                stripped.split('\'').next().unwrap_or("")
            } else {
                rest.split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or("")
            };
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        search = tag_end;
    }
    None
}

fn article_copy_url(article: &Article) -> &str {
    let url = article.url.trim();
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let bare_root = without_query
        .split_once("://")
        .is_some_and(|(_, rest)| rest.split_once('/').is_none_or(|(_, path)| path.is_empty()));
    if has_direct_media_link(article) && (url.is_empty() || bare_root) {
        article.media_url.as_deref().unwrap_or("").trim()
    } else {
        url
    }
}

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

/// Python core.categories.is_uncategorized: no category, blank,
/// or the "Uncategorized" sentinel (any case).
fn is_uncategorized_cat(category: Option<&str>) -> bool {
    let c = category.unwrap_or("").trim();
    c.is_empty() || c.eq_ignore_ascii_case("uncategorized")
}

/// Python `gui/mainframe.py::_is_protected_uncategorized` (Audit
/// 8 D11): the sentinel is protected only when the active
/// provider treats it as the synthetic "no category" bucket.
/// On Miniflux it is a real category and the rename/remove
/// guards step aside.
fn is_protected_uncategorized(category: Option<&str>) -> bool {
    if !is_uncategorized_cat(category) {
        return false;
    }
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    !blindrss_core::provider::uncategorized_is_real_category(&cfg)
}

/// Remap the global config `delete_behavior` category target
/// through a category path mapping (Python
/// `LocalProvider._update_category_delete_behavior`).
fn remap_global_delete_behavior(mapping: &[(String, String)]) {
    if mapping.is_empty() {
        return;
    }
    let mut cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let current = cfg.get_str("delete_behavior", "").to_string();
    if let Some(new) = blindrss_core::remap_delete_behavior(&current, mapping) {
        cfg.set_path("delete_behavior", serde_json::json!(new));
        let _ = cfg.save(&config_path());
    }
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
    if is_uncategorized_cat(Some(path)) {
        return feeds
            .iter()
            .filter(|f| is_uncategorized_cat(f.category.as_deref()))
            .cloned()
            .collect();
    }
    let prefix = format!("{path}{}", blindrss_core::CATEGORY_PATH_SEP);
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

fn feed_label(counts: &std::collections::HashMap<String, i64>, feed: &Feed) -> String {
    match counts.get(&feed.id) {
        Some(&n) if n > 0 => format!("{} ({n})", feed.title),
        _ => feed.title.clone(),
    }
}

/// Fill a list row's missing article body from the database.
/// The list loads are light (`Db::articles_light`: no bodies);
/// anything that actually reads a body resolves it here, once,
/// through the primary-key lookup.
fn ensure_content(db: Option<&Db>, a: &mut Article) {
    if a.content.is_none()
        && let Some(db) = db
        && let Ok(Some(full)) = db.article_by_id(&a.id)
    {
        a.content = full.content;
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
    // One grouped count query for the whole tree (Python's
    // shape). Per-feed COUNTs here were the ~30 s startup on
    // large libraries: every feed was counted once for its
    // category node, again for its own label, and the counts
    // ran as full table scans (2026-11-02 perf audit).
    let counts = db.unread_counts().unwrap_or_default();
    let feeds = state.feeds.clone();
    // Category grouping (Python parity): a node per category path
    // ("News", "News / Tech"), feeds under their category node,
    // uncategorized feeds at the root. Node data is "cat:<path>".
    let expand_cats = blindrss_core::config::Config::load(&config_path())
        .unwrap_or_default()
        .get_bool("category_tree_default_expanded", false);
    let mut paths: Vec<String> = db
        .categories()
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.title)
        .filter(|t| !is_uncategorized_cat(Some(t)))
        .collect();
    for f in &feeds {
        if let Some(cat) = &f.category
            && !cat.trim().is_empty()
            && !is_uncategorized_cat(Some(cat))
            && !paths.contains(cat)
        {
            paths.push(cat.clone());
        }
    }
    paths.sort();
    let mut cat_items: std::collections::HashMap<String, TreeItemId> =
        std::collections::HashMap::new();
    // Built-in Favorites view (Audit 8 G06): Python adds the
    // node whenever the provider supports favorites, which
    // every backend does. Item data is the shared view id.
    tree.append_item_with_data(&root, "Favorites", "favorites:all".to_string(), None, None);
    // Uncategorized is a real node (Python's local provider):
    // feeds with no category — or carrying the sentinel — group
    // under it, and selecting it lists only their articles.
    let uncat_feeds: Vec<&Feed> = feeds
        .iter()
        .filter(|f| is_uncategorized_cat(f.category.as_deref()))
        .collect();
    if !uncat_feeds.is_empty() {
        let unread: i64 = uncat_feeds
            .iter()
            .map(|f| counts.get(&f.id).copied().unwrap_or(0))
            .sum();
        let label = if unread > 0 {
            format!("Uncategorized ({unread})")
        } else {
            "Uncategorized".to_string()
        };
        if let Some(item) =
            tree.append_item_with_data(&root, &label, "cat:Uncategorized".to_string(), None, None)
        {
            cat_items.insert("Uncategorized".to_string(), item);
        }
    }
    for path in &paths {
        let mut cur = String::new();
        for seg in path
            .split(blindrss_core::CATEGORY_PATH_SEP)
            .filter(|s| !s.trim().is_empty())
        {
            let parent = if cur.is_empty() {
                root.clone()
            } else {
                cat_items.get(&cur).cloned().unwrap_or_else(|| root.clone())
            };
            if !cur.is_empty() {
                cur.push_str(blindrss_core::CATEGORY_PATH_SEP);
            }
            cur.push_str(seg.trim());
            if !cat_items.contains_key(&cur) {
                // Node label: leaf name plus the unread total of
                // every feed in scope, Python-style.
                let prefix = format!("{cur}{}", blindrss_core::CATEGORY_PATH_SEP);
                let unread: i64 = feeds
                    .iter()
                    .filter(|f| {
                        f.category.as_deref() == Some(cur.as_str())
                            || f.category
                                .as_deref()
                                .is_some_and(|c| c.starts_with(&prefix))
                    })
                    .map(|f| counts.get(&f.id).copied().unwrap_or(0))
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
        let label = feed_label(&counts, f);
        let parent = if is_uncategorized_cat(f.category.as_deref()) {
            cat_items
                .get("Uncategorized")
                .cloned()
                .unwrap_or_else(|| root.clone())
        } else {
            f.category
                .as_ref()
                .and_then(|c| cat_items.get(c))
                .cloned()
                .unwrap_or_else(|| root.clone())
        };
        tree.append_item_with_data(&parent, &label, f.id.clone(), None, None);
    }
    // Expansion (Python issue #33): the user's explicit choices
    // survive rebuilds; the configured default covers the rest.
    for (path, item) in &cat_items {
        let expanded = if state.collapsed_cats.contains(path) {
            false
        } else if state.expanded_cats.contains(path) {
            true
        } else {
            expand_cats
        };
        if expanded {
            tree.expand(item);
        }
    }
    // YouTube Recommendations virtual feed (the account
    // provider wrapper injects it whenever signed in AND a
    // recommendations snapshot exists — Python's _has_snapshot).
    let yt_token = blindrss_core::config::Config::load(&config_path())
        .unwrap_or_default()
        .get_str("youtube_account_refresh_token", "")
        .to_string();
    if !yt_token.is_empty() {
        let account = blindrss_core::youtube_account::account_key(&yt_token);
        if db.has_recommendation_snapshot(&account).unwrap_or(false) {
            tree.append_item_with_data(
                &root,
                "Recommendations",
                blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID.to_string(),
                None,
                None,
            );
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

/// (Re)build the article list's columns from the configured
/// layout (core/article_columns.rs). Title is pinned first.
fn setup_article_columns(list: &ListCtrl) {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let layout = blindrss_core::article_columns::layout_from_config(&cfg);
    list.clear_all();
    for (i, col) in blindrss_core::article_columns::visible_columns(&layout)
        .iter()
        .enumerate()
    {
        list.insert_column(i as i64, &col.label, ListColumnFormat::Left, col.width);
    }
}

/// One article cell for a column key.
fn article_cell(
    a: &Article,
    key: &str,
    feed_titles: &std::collections::HashMap<&str, &str>,
) -> String {
    match key {
        "author" => a.author.clone().unwrap_or_default(),
        "media" => a
            .media_type
            .clone()
            .filter(|t| !t.is_empty())
            .or_else(|| {
                if a.media_url.as_deref().is_some_and(|m| !m.is_empty()) {
                    Some("Media".to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default(),
        "date" => a.date.clone().unwrap_or_default(),
        "feed" => feed_titles
            .get(a.feed_id.as_str())
            .copied()
            .unwrap_or(&a.feed_id)
            .to_string(),
        "description" => a
            .description
            .as_deref()
            .map(blindrss_core::extract::html_to_text)
            .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        "status" => {
            let mut parts = vec![if a.is_read { "Read" } else { "Unread" }];
            if a.is_favorite {
                parts.push("Favorite");
            }
            parts.join(", ")
        }
        _ => a.title.clone(),
    }
}

/// Article-list page size (Python's `article_page_size`).
const ARTICLE_PAGE: usize = 400;

/// The current article-list view as a core query (Audit 8
/// Task 11): tree scope + read/media filters + display sort +
/// active search, composed in SQL by
/// `Db::articles_view_page`, so every view pages in actual
/// display order — the Audit 7 gate that only let the
/// unfiltered date-descending view page is gone (G03/G04).
fn view_query(state: &State) -> blindrss_core::db::ArticleQuery {
    use blindrss_core::db::ViewScope;
    let scope = match state.selected_feed.as_deref() {
        None => ViewScope::All,
        Some("favorites:all") => ViewScope::Favorites,
        Some(f) => match f.strip_prefix("cat:") {
            Some(path) => ViewScope::Category(path.to_string()),
            None => match f.strip_prefix("smart:") {
                Some(id) => ViewScope::Smart(id.to_string()),
                None => ViewScope::Feed(f.to_string()),
            },
        },
    };
    blindrss_core::db::ArticleQuery {
        scope,
        read_filter: state.read_filter,
        media_filter: state.media_filter,
        sort_by: state.sort_by.clone(),
        ascending: state.sort_ascending,
        search_terms: state.search_terms.clone(),
        search_title_only: state.search_title_only,
    }
}

/// Python's `_article_search_text` + `_filter_articles` as an
/// in-memory predicate, for the whole-load views (merged
/// recommendations, the recommendations view) that bypass the
/// SQL view query: EVERY term must appear in the title, body,
/// description, author, article/media URLs or feed title —
/// titles only in title_only mode.
fn article_matches_terms(
    a: &Article,
    terms: &[String],
    title_only: bool,
    feed_titles: &std::collections::HashMap<&str, &str>,
) -> bool {
    if terms.is_empty() {
        return true;
    }
    let title = a.title.to_lowercase();
    if title_only {
        return terms.iter().all(|t| title.contains(t.as_str()));
    }
    let mut text = title;
    for part in [
        a.content.as_deref(),
        a.description.as_deref(),
        a.author.as_deref(),
        Some(a.url.as_str()),
        a.media_url.as_deref(),
        feed_titles.get(a.feed_id.as_str()).copied(),
    ]
    .into_iter()
    .flatten()
    {
        text.push(' ');
        text.push_str(&part.to_lowercase());
    }
    terms.iter().all(|t| text.contains(t.as_str()))
}

/// Display-order sort for whole-load views (merged
/// recommendations, the recommendations view): Python's
/// `_sort_articles_for_display` — stabilize by recency
/// (date DESC, id DESC), then a STABLE primary-key sort whose
/// direction flips only the key comparison, so ties keep
/// recency order in both directions. (The old code reversed
/// the whole vec for descending, flipping ties too.) Paged
/// views never come through here: their order is the SQL
/// ORDER BY in core, which encodes the same rule.
fn sort_for_display(
    loaded: &mut [Article],
    sort_by: &str,
    ascending: bool,
    feed_titles: &std::collections::HashMap<&str, &str>,
) {
    loaded.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
    fn keyed(loaded: &mut [Article], ascending: bool, key: impl Fn(&Article) -> String) {
        if ascending {
            loaded.sort_by_key(&key);
        } else {
            loaded.sort_by_key(|a| std::cmp::Reverse(key(a)));
        }
    }
    match sort_by {
        "date" => {
            if ascending {
                loaded.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.cmp(&b.id)));
            }
        }
        "name" => keyed(loaded, ascending, |a| a.title.trim().to_lowercase()),
        "author" => keyed(loaded, ascending, |a| {
            a.author.as_deref().unwrap_or("").trim().to_lowercase()
        }),
        "description" => keyed(loaded, ascending, |a| {
            article_cell(a, "description", feed_titles).to_lowercase()
        }),
        "status" => keyed(loaded, ascending, |a| {
            if a.is_read { "read" } else { "unread" }.to_string()
        }),
        "feed" => keyed(loaded, ascending, |a| {
            feed_titles
                .get(a.feed_id.as_str())
                .copied()
                .unwrap_or("")
                .trim()
                .to_lowercase()
        }),
        _ => {}
    }
}

/// Insert list rows for `rows` starting at display index
/// `start` (shared by the refill fill and Load More appends).
fn insert_article_rows(
    list: &ListCtrl,
    rows: &[Article],
    start: usize,
    feed_titles: &std::collections::HashMap<&str, &str>,
    visible: &[&blindrss_core::article_columns::ColumnLayout],
) {
    for (j, a) in rows.iter().enumerate() {
        let i = (start + j) as i64;
        list.insert_item(i, &a.title, None);
        for (col_idx, col) in visible.iter().enumerate().skip(1) {
            let cell = article_cell(a, &col.key, feed_titles);
            list.set_item_text_by_column(i, col_idx as i32, &cell);
        }
    }
}

/// Append the next page of the current view (activation of
/// the "Load more items (Enter)" placeholder row).
fn load_more_articles(state: &mut State, list: &ListCtrl) {
    let offset = state.articles.len();
    if state.view_total <= offset as i64 {
        return;
    }
    let Some(db) = state.db.as_ref() else { return };
    // The same composed view query as the refill: the next page
    // continues the exact display order the first page started,
    // whatever filter/sort/search is active (Audit 8 G03/G04).
    let page = db
        .articles_view_page(&view_query(state), ARTICLE_PAGE, offset)
        .unwrap_or_default();
    list.freeze();
    // The placeholder row sits at `offset`; it goes first.
    list.delete_item(offset as i64);
    if page.is_empty() {
        // The library shrank under the view (deletions since
        // the count was taken): stop offering more.
        state.view_total = offset as i64;
        list.thaw();
        return;
    }
    let feed_titles: std::collections::HashMap<&str, &str> = state
        .feeds
        .iter()
        .map(|f| (f.id.as_str(), f.title.as_str()))
        .collect();
    let layout = blindrss_core::article_columns::layout_from_config(
        &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
    );
    let visible = blindrss_core::article_columns::visible_columns(&layout);
    insert_article_rows(list, &page, offset, &feed_titles, &visible);
    state.articles.extend(page);
    if state.view_total > state.articles.len() as i64 {
        let at = state.articles.len() as i64;
        list.insert_item(at, &tr("Load more items (Enter)"), None);
    }
    list.thaw();
    // Land the user on the first new row.
    list.set_item_state(
        offset as i64,
        ListItemState::Selected | ListItemState::Focused,
        ListItemState::Selected | ListItemState::Focused,
    );
    list.ensure_visible(offset as i64);
}

/// The signed-in YouTube account key from a loaded config
/// ("" when signed out).
fn youtube_account_key(cfg: &blindrss_core::config::Config) -> String {
    blindrss_core::youtube_account::account_key(cfg.get_str("youtube_account_refresh_token", ""))
}

/// Whether a category view includes the YouTube account's
/// recommendations (Python's wrapper `_view`): the account
/// category itself, or any ancestor of it.
fn category_includes_recommendations(cfg: &blindrss_core::config::Config, path: &str) -> bool {
    let category = cfg.get_str("youtube_account_category", "YouTube");
    let category = if category.is_empty() {
        "YouTube"
    } else {
        category
    };
    category == path || category.starts_with(&format!("{path}{}", blindrss_core::CATEGORY_PATH_SEP))
}

/// Which recommendations a Mark All Read over a view covers,
/// as (account, favorites_only) — Python's wrapper
/// `mark_all_read` marks `_articles(view)` beside the provider
/// mark (Audit 8 D20). None when the view includes no
/// recommendations or no account is signed in.
fn recommendations_mark_scope(
    cfg: &blindrss_core::config::Config,
    filter: Option<&str>,
) -> Option<(String, bool)> {
    let account = youtube_account_key(cfg);
    if account.is_empty() {
        return None;
    }
    match filter {
        None => Some((account, false)),
        Some("favorites:all") => Some((account, true)),
        Some(id) if id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID => {
            Some((account, false))
        }
        Some(view) => {
            let path = view.strip_prefix("cat:")?;
            category_includes_recommendations(cfg, path).then_some((account, false))
        }
    }
}

/// The confirmation prompt for a Mark All Read wrapper
/// (Python's `_confirm_and_mark_all_read` call sites): the
/// main command asks globally; tree context commands name
/// their node — the feed variant falls back to "this feed"
/// when the feed has no title.
fn mark_all_prompt(view: Option<&str>, feed_title: Option<&str>) -> String {
    match view {
        None => "Mark all items in all feeds as read?".to_string(),
        Some(v) => match v.strip_prefix("cat:") {
            Some(path) => format!("Mark all items in category \"{path}\" as read?"),
            None => match feed_title {
                Some(title) if !title.trim().is_empty() => {
                    format!("Mark all items in \"{title}\" as read?")
                }
                _ => "Mark all items in this feed as read?".to_string(),
            },
        },
    }
}

/// Python's Mark All confirmation (wx.MessageBox YES_NO,
/// title "Mark All as Read"): anything but Yes cancels, and
/// the wrappers run this before touching any state, so a
/// cancel provably changes nothing (Audit 8 G05).
fn confirm_mark_all(frame: &Frame, prompt: &str) -> bool {
    let md = MessageDialog::builder(frame, prompt, "Mark All as Read")
        .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
        .build();
    let answer = md.show_modal();
    md.destroy();
    answer == ID_YES
}

/// The single Mark All Read operation behind the wrappers
/// (Audit 8 G05): marks exactly the given view scope — None is
/// the global scope. Hosted providers mark remotely on a
/// worker first (feed/category/global route through the
/// provider trait, Audit 8 D23); every other scope, and the
/// local provider entirely, resolves in core
/// (`Db::mark_view_read`), with the recommendations in scope
/// marked in their own table beside it (Audit 8 D20).
fn run_mark_all(
    state: &Rc<RefCell<State>>,
    frame: &Frame,
    tx: &Sender<RefreshMsg>,
    view: Option<String>,
) {
    {
        let st = state.borrow();
        let filter = view.clone();
        let is_feed_view = filter.as_deref().is_some_and(|f| {
            !f.starts_with("cat:")
                && !f.starts_with("smart:")
                && f != "favorites:all"
                && f != blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID
        });
        let is_cat_view = filter.as_deref().is_some_and(|f| f.starts_with("cat:"));
        if filter.is_none() || is_feed_view || is_cat_view {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if blindrss_core::provider::active_name(&cfg) != "local" {
                // Recommendations are local-only even under a
                // hosted provider (Python's wrapper marks them
                // beside the remote mark — Audit 8 D20).
                if let Some(db) = st.db.as_ref()
                    && let Some((account, favorites_only)) =
                        recommendations_mark_scope(&cfg, filter.as_deref())
                {
                    let _ = db.mark_recommendations_read(&account, favorites_only);
                }
                drop(st);
                frame.set_status_text("Marking all read...", 0);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let cfg =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                    let result = match filter.as_deref() {
                        None => provider.mark_all_read(None),
                        Some(view) if view.starts_with("cat:") => {
                            provider.mark_category_read(view.strip_prefix("cat:").unwrap_or(""))
                        }
                        Some(feed_id) => provider.mark_all_read(Some(feed_id)),
                    };
                    let _ = tx.send(RefreshMsg::ProviderAction(
                        result.map(|n| format!("Marked {n} articles read")),
                    ));
                });
                return;
            }
        }
    }
    let mut st = state.borrow_mut();
    let mut count = 0;
    if let Some(db) = st.db.as_ref() {
        count = db.mark_view_read(view.as_deref()).unwrap_or(0);
        // Recommendations in scope are marked in their own
        // table, persisted (Audit 8 D20 — Python's wrapper
        // mark_all_read).
        let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
        if let Some((account, favorites_only)) = recommendations_mark_scope(&cfg, view.as_deref()) {
            count += db
                .mark_recommendations_read(&account, favorites_only)
                .unwrap_or(0);
        }
        // The wrappers' scopes always cover the displayed view
        // (global covers everything; the context scope IS the
        // current view), so the shown rows flip with the store.
        for a in st.articles.iter_mut() {
            a.is_read = true;
        }
    }
    frame.set_status_text(&format!("Marked {count} articles read"), 0);
}

/// Merge recommendation rows into an article list and put the
/// whole list in the standard display order (date DESC, id
/// DESC — the order the SQL queries return). Python's wrapper
/// concatenates its rows into the provider's list and the
/// merged list sorts by the stored (first-recommended) dates.
fn merge_recommendations(mut base: Vec<Article>, extra: Vec<Article>) -> Vec<Article> {
    if extra.is_empty() {
        return base;
    }
    base.extend(extra);
    base.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
    base
}

fn refill_articles(state: &mut State, list: &ListCtrl) {
    state.reader_generation = state.reader_generation.wrapping_add(1);
    state.pending_fulltext = None;
    // Freeze/Thaw around the whole fill: without it every
    // inserted row repaints the control — arrowing between
    // categories froze for ~1s at a time on big categories.
    // Python wraps its fills the same way
    // (gui/reader_performance.py).
    list.freeze();
    refill_inner(state, list);
    list.thaw();
}

fn refill_inner(state: &mut State, list: &ListCtrl) {
    // Columns are rebuilt only when the layout config changed;
    // the common refill just clears the rows (clear_all also
    // tears down and recreates every column).
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let sig = serde_json::to_string(
        cfg.raw()
            .get("article_columns")
            .unwrap_or(&serde_json::Value::Null),
    )
    .unwrap_or_default();
    if sig != state.cols_sig {
        setup_article_columns(list);
        state.cols_sig = sig;
    } else {
        list.delete_all_items();
    }
    state.articles.clear();
    // Paging state belongs to the view being built; only the
    // paged paths below re-arm it.
    state.view_total = -1;
    let Some(db) = state.db.as_ref() else { return };
    let filter = state.selected_feed.as_deref();
    // YouTube Recommendations virtual feed: its rows live in
    // their own table, so it keeps the whole-load path (with
    // the in-memory filter/sort/search pipeline).
    if filter == Some(blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID) {
        let token = blindrss_core::config::Config::load(&config_path())
            .unwrap_or_default()
            .get_str("youtube_account_refresh_token", "")
            .to_string();
        let account = blindrss_core::youtube_account::account_key(&token);
        let loaded = db.recommendations(&account).unwrap_or_default();
        return finish_refill(state, list, loaded);
    }
    // Merged-recommendation views (Audit 8 D19) load whole and
    // date-sort like Python's wrapper, so they do not page; the
    // merge base is no longer capped either (Audit 8 G03) — the
    // whole view loads, then finish_refill's pipeline applies
    // search/filters/sort in memory, recommendations included.
    let account = youtube_account_key(&cfg);
    // Favorites merge whenever favorite recommendations exist
    // (Audit 8 G06/D19 — including favorites that fell out of
    // the latest snapshot, so no snapshot gate here).
    if filter == Some("favorites:all") && !account.is_empty() {
        let favs = db.favorite_recommendations(&account).unwrap_or_default();
        if !favs.is_empty() {
            let loaded = db.favorites(WHOLE_VIEW).unwrap_or_default();
            return finish_refill(state, list, merge_recommendations(loaded, favs));
        }
    }
    if !account.is_empty() && db.has_recommendation_snapshot(&account).unwrap_or(false) {
        if let Some(path) = filter.and_then(|f| f.strip_prefix("cat:"))
            && category_includes_recommendations(&cfg, path)
        {
            let loaded = db
                .articles_for_category_light(path, WHOLE_VIEW)
                .unwrap_or_default();
            let recs = db.recommendations_merged(&account).unwrap_or_default();
            return finish_refill(state, list, merge_recommendations(loaded, recs));
        }
        if filter.is_none() {
            let loaded = db.articles_light(None, WHOLE_VIEW).unwrap_or_default();
            let recs = db.recommendations_merged(&account).unwrap_or_default();
            if !recs.is_empty() {
                return finish_refill(state, list, merge_recommendations(loaded, recs));
            }
        }
    }
    // Every other view — All, feed, category (Python's
    // membership clause lives in the core query), favorites,
    // smart folder — pages through the unified view query in
    // actual display order (Audit 8 G03/G04).
    let query = view_query(state);
    state.view_total = db.articles_view_count(&query).unwrap_or(-1);
    let loaded = db
        .articles_view_page(&query, ARTICLE_PAGE, 0)
        .unwrap_or_default();
    finish_refill_ordered(state, list, loaded);
}

/// Whole-view load bound for the merged-recommendation paths:
/// deliberately not a display cap (Audit 8 G03 removed the
/// 300/500 ceilings) — merged views load whole by design
/// (Audit 8 D19), and this only guards against a pathological
/// unbounded read.
const WHOLE_VIEW: usize = 10_000_000;

/// Whole-load pipeline: the in-view search, the view filters
/// (read + media, Python's Ctrl+1..6) and the display sort run
/// in memory over the fully loaded view. Used by the views
/// that load whole (merged recommendations, the
/// recommendations view); paged views compose all of it in
/// SQL instead (`finish_refill_ordered`).
fn finish_refill(state: &mut State, list: &ListCtrl, mut loaded: Vec<Article>) {
    let feed_titles: std::collections::HashMap<&str, &str> = state
        .feeds
        .iter()
        .map(|f| (f.id.as_str(), f.title.as_str()))
        .collect();
    if !state.search_terms.is_empty() {
        let terms = state.search_terms.clone();
        let title_only = state.search_title_only;
        loaded.retain(|a| article_matches_terms(a, &terms, title_only, &feed_titles));
    }
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
    sort_for_display(
        &mut loaded,
        &state.sort_by,
        state.sort_ascending,
        &feed_titles,
    );
    render_refill_rows(state, list, loaded);
}

/// Paged pipeline: the core view query already applied scope,
/// search, filters and display order — render rows as
/// returned, no second in-memory pass (a second pass over one
/// page would corrupt the cross-page order).
fn finish_refill_ordered(state: &mut State, list: &ListCtrl, loaded: Vec<Article>) {
    render_refill_rows(state, list, loaded);
}

/// Render a finished article set into the list and arm the
/// paging placeholder when the view has more rows.
fn render_refill_rows(state: &mut State, list: &ListCtrl, loaded: Vec<Article>) {
    let layout = blindrss_core::article_columns::layout_from_config(
        &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
    );
    let visible = blindrss_core::article_columns::visible_columns(&layout);
    let feed_titles: std::collections::HashMap<&str, &str> = state
        .feeds
        .iter()
        .map(|f| (f.id.as_str(), f.title.as_str()))
        .collect();
    insert_article_rows(list, &loaded, 0, &feed_titles, &visible);
    state.articles = loaded;
    // Paged view with rows still unloaded: Python's trailing
    // placeholder row. It is a display row only — selection
    // plumbing resolves articles through `articles`, where the
    // placeholder index simply has no entry.
    if state.view_total > state.articles.len() as i64 {
        let at = state.articles.len() as i64;
        list.insert_item(at, &tr("Load more items (Enter)"), None);
    }
}

/// UI language: config `language` ("auto"/missing = OS locale),
/// normalised to a primary tag ("de_DE.UTF-8" -> "de").
/// Translation catalog for the UI, loaded once from the locale
/// tree (exe_dir/locale in releases, the repo's locale/ in dev)
/// for the configured language. Strings without a catalog entry
/// fall back to English (Python's gettext behaviour).
static I18N: std::sync::OnceLock<blindrss_core::i18n::Catalog> = std::sync::OnceLock::new();

/// Bundled locale roots, best first (the exe's locale/ in
/// releases, the repo's locale/ in dev).
pub fn locale_roots() -> Vec<std::path::PathBuf> {
    let mut roots = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../locale")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        roots.insert(0, dir.join("locale"));
    }
    roots
}

pub fn tr(text: &str) -> String {
    let catalog = I18N.get_or_init(|| {
        let lang = ui_language();
        // Downloaded translation updates shadow the bundled
        // catalogs (translation_updates.override_root).
        let mut roots = vec![blindrss_core::translation_updates::override_root(
            &data_dir(),
        )];
        roots.extend(locale_roots());
        for root in roots {
            let cat = blindrss_core::i18n::Catalog::load(&root, &lang);
            if !cat.is_empty() {
                return cat;
            }
        }
        blindrss_core::i18n::Catalog::default()
    });
    catalog.get(text).to_string()
}

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
    // Release layout: locale/ ships beside the executable; dev builds
    // use the repo's locale/ tree. Both come from locale_roots(), so
    // the startup labels and tr() always read the same catalog — the
    // old dev fallback pointed at a directory that does not exist.
    locale_roots()
        .into_iter()
        .find(|r| r.exists())
        .unwrap_or_else(app_dir)
}

/// One pass of the background watcher (Python's
/// CookieImportWatcher tick + the translation-update scheduler):
/// hands-free cookie imports from Downloads and browser profiles,
/// plus a due translation-catalog check. Runs on a worker thread;
/// findings land in the status bar via StatusNote.
fn run_watcher_once(tx: &std::sync::mpsc::Sender<RefreshMsg>) {
    use blindrss_core::cookies_import as ci;
    use blindrss_core::translation_updates as tu;
    let cfg_path = config_path();
    let mut cfg = blindrss_core::config::Config::load(&cfg_path).unwrap_or_default();
    let dir = data_dir();
    let jar = dir.join(ci::SITE_JAR_FILENAME);
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cookie_markers = |cfg: &blindrss_core::config::Config| {
        (
            cfg.raw().get("site_cookies_profile_mtimes").cloned(),
            cfg.raw().get("site_cookies_full_profile_mtimes").cloned(),
            cfg.raw().get("site_cookies_full_import_last_run").cloned(),
        )
    };
    let markers_before = cookie_markers(&cfg);
    let mut notes: Vec<String> = Vec::new();
    let mut dirty = false;
    if ci::auto_import_youtube_cookies(&mut cfg, &dir, None, now_s).is_some() {
        notes.push("YouTube cookies imported for yt-dlp".to_string());
        dirty = true;
    }
    if ci::auto_import_downloads(&mut cfg, &jar, None, now_s).is_some() {
        notes.push("Site cookies updated from a Downloads export".to_string());
        dirty = true;
    }
    let stored = ci::auto_import_browser_profiles(&mut cfg, &jar, now_s as i64);
    if stored > 0 {
        notes.push(format!(
            "Site cookies refreshed from browser profiles ({stored} cookies)"
        ));
        dirty = true;
    }
    if cookie_markers(&cfg) != markers_before {
        dirty = true;
    }
    if cfg.get_bool(tu::CFG_ENABLED, true) {
        let frequency = cfg
            .get_str(tu::CFG_FREQUENCY, tu::DEFAULT_FREQUENCY)
            .to_string();
        if tu::is_due(&dir, &frequency, now_s as i64) {
            let mut langs: Vec<String> = Vec::new();
            for root in locale_roots() {
                if let Ok(entries) = std::fs::read_dir(&root) {
                    for entry in entries.flatten() {
                        if entry.path().is_dir()
                            && let Some(name) = entry.file_name().to_str()
                            && name != "en"
                            && !langs.contains(&name.to_string())
                        {
                            langs.push(name.to_string());
                        }
                    }
                }
            }
            let result =
                tu::check_and_update(&dir, &locale_roots(), &langs, &tu::HttpFetch, now_s as i64);
            if result.changed() {
                notes.push(format!(
                    "Translation updates installed for {}; they apply on next launch",
                    result.updated.join(", ")
                ));
            }
        }
    }
    if dirty {
        let _ = cfg.save(&cfg_path);
    }
    if !notes.is_empty() {
        let _ = tx.send(RefreshMsg::StatusNote(notes.join("; ")));
    }
}

fn main() {
    // Packaged runtime smoke test (Python main.py's
    // --blindrss-runtime-self-test, core/runtime_self_test.py;
    // Audit 8 O15): verify the bundled tools resolve and run and
    // the extraction cascade is healthy, print failures, and exit
    // without opening the GUI.
    if std::env::args().any(|a| a == "--blindrss-runtime-self-test") {
        let errors = blindrss_core::dependency::runtime_smoke_test();
        if errors.is_empty() {
            println!("Runtime self-test passed.");
            return;
        }
        for e in &errors {
            eprintln!("Runtime self-test failed: {e}");
        }
        std::process::exit(1);
    }
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
    // Translation overrides from a previous version stand down
    // before the catalog loads (translation_updates).
    blindrss_core::translation_updates::prepare_overrides_for_app_version(
        &data_dir(),
        &locale_roots(),
        env!("CARGO_PKG_VERSION"),
    );
    // Single instance: hand activation to the running copy
    // and exit when one answers.
    if blindrss_core::single_instance::signal_existing() {
        log_line("BlindRSS already running; activation sent, exiting");
        return;
    }
    let _ = wxdragon::main(|_| {
        apply_http_config();
        startup_maintenance();
        let language = ui_language();
        let catalog = blindrss_core::i18n::Catalog::load(&locale_root(), &language);
        let refresh_label = catalog.get("Refresh All Feeds").to_string();
        let frame = Frame::builder()
            .with_title("BlindRSS")
            .with_size(Size::new(1100, 700))
            .build();
        frame.create_status_bar(1, 0, -1, "status");

        // Tray icon (orange square placeholder artwork; the real
        // app icon ships with the release packaging) with the
        // tray behaviours: popup menu (Show / Refresh All /
        // Exit), double-click restores, close_to_tray vetoes the
        // window close into a hide, start_in_system_tray starts
        // hidden. The handle is shared with the refresh drain
        // for Windows balloon notifications.
        let tray = TaskBarIcon::builder().build();
        {
            let mut rgba = Vec::with_capacity(32 * 32 * 4);
            for _ in 0..32 * 32 {
                rgba.extend_from_slice(&[240, 130, 20, 255]);
            }
            if let Some(bmp) = Bitmap::from_rgba(&rgba, 32, 32) {
                tray.set_icon(&bmp, "BlindRSS");
            }
        }
        const TRAY_SHOW: i32 = 2301;
        const TRAY_REFRESH: i32 = 2302;
        const TRAY_EXIT: i32 = 2303;
        const TRAY_UNREAD: i32 = 2304;
        const TRAY_PLAYER_TOGGLE: i32 = 2305;
        const TRAY_PLAY_PAUSE: i32 = 2306;
        const TRAY_STOP: i32 = 2307;
        const TRAY_REWIND: i32 = 2308;
        const TRAY_FF: i32 = 2309;
        const TRAY_VOL_UP: i32 = 2310;
        const TRAY_VOL_DOWN: i32 = 2311;
        // Tray menu matches Python's gui/tray.py (Batch 41a /
        // Audit 6 G1.4): an "Unread: {count}" label, Restore,
        // Show/Hide Player, Refresh Feeds, then the player
        // transport set. The unread label is refreshed by the
        // drain tick below whenever the total changes.
        let tray_unread0 = Db::open(&db_path())
            .ok()
            .and_then(|db| db.unread_count(None).ok())
            .unwrap_or(0);
        let mut tray_menu = Menu::builder()
            .append_item(TRAY_UNREAD, &format!("Unread: {tray_unread0}"), "")
            .append_item(TRAY_SHOW, "&Restore", "")
            .append_item(TRAY_PLAYER_TOGGLE, "Show/Hide &Player", "")
            .append_separator()
            .append_item(TRAY_REFRESH, "Refresh &Feeds", "")
            .append_separator()
            .append_item(TRAY_PLAY_PAUSE, "Play/Pause", "")
            .append_item(TRAY_STOP, "Stop", "")
            .append_item(TRAY_REWIND, "Rewind", "")
            .append_item(TRAY_FF, "Fast Forward", "")
            .append_item(TRAY_VOL_UP, "Volume Up", "")
            .append_item(TRAY_VOL_DOWN, "Volume Down", "")
            .append_separator()
            .append_item(TRAY_EXIT, "E&xit", "")
            .build();
        tray_menu.enable_item(TRAY_UNREAD, false);
        tray.set_popup_menu(&mut tray_menu);
        let tray_menu = Rc::new(RefCell::new(tray_menu));
        // Set once the real actions exist (below); the tray menu
        // and close handler consult these slots.
        type ActionSlot = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
        let tray_refresh_slot: ActionSlot = Rc::new(RefCell::new(None));
        let tray_play_slot: ActionSlot = Rc::new(RefCell::new(None));
        let quit_flag = Rc::new(std::cell::Cell::new(false));
        {
            // wx has no taskbar mouse events on macOS; the tray
            // menu's Show item is the restore path there.
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                let frame_tray = frame;
                tray.on_left_double_click(move |_| {
                    frame_tray.iconize(false);
                    frame_tray.show(true);
                    frame_tray.raise();
                });
            }
        }
        {
            let frame_tray = frame;
            let slot = tray_refresh_slot.clone();
            let play_slot = tray_play_slot.clone();
            let quit = quit_flag.clone();
            tray.on_menu(move |ev| match ev.get_id() {
                x if x == TRAY_SHOW => {
                    frame_tray.iconize(false);
                    frame_tray.show(true);
                    frame_tray.raise();
                }
                x if x == TRAY_REFRESH => {
                    if let Some(action) = slot.borrow().as_ref() {
                        action();
                    }
                }
                x if x == TRAY_PLAYER_TOGGLE => {
                    player::dispatch_command("player.show_hide");
                }
                x if x == TRAY_PLAY_PAUSE => {
                    if !player::dispatch_command("player.play_pause")
                        && let Some(action) = play_slot.borrow().as_ref()
                    {
                        action();
                    }
                }
                x if x == TRAY_STOP => {
                    player::dispatch_command("player.stop");
                }
                x if x == TRAY_REWIND => {
                    player::dispatch_command("player.rewind");
                }
                x if x == TRAY_FF => {
                    player::dispatch_command("player.forward");
                }
                x if x == TRAY_VOL_UP => {
                    player::dispatch_command("player.volume_up");
                }
                x if x == TRAY_VOL_DOWN => {
                    player::dispatch_command("player.volume_down");
                }
                x if x == TRAY_EXIT => {
                    quit.set(true);
                    frame_tray.close(true);
                }
                _ => {}
            });
        }
        {
            // close_to_tray: veto the close and hide instead,
            // unless the quit flag says this close is a real exit
            // (tray Exit or File > Exit).
            let frame_tray = frame;
            let quit = quit_flag.clone();
            frame.bind_internal(EventType::CLOSE_WINDOW, move |ev| {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if cfg.get_bool("close_to_tray", false) && !quit.get() {
                    ev.veto();
                    frame_tray.show(false);
                }
            });
        }
        let tray = Rc::new(tray);

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
        let search_box = TextCtrl::builder(&frame)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        msaa_names::set_name(&search_box, &tr("Search"));
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

        let (tx, rx): (Sender<RefreshMsg>, Receiver<RefreshMsg>) = channel();
        let splitter = SplitterWindow::builder(&frame).build();
        let tree = TreeCtrl::builder(&splitter).build();
        msaa_names::set_name(&tree, &tr("Feeds and categories"));
        let right = Panel::builder(&splitter).build();
        let list = ListCtrl::builder(&right)
            .with_style(ListCtrlStyle::Report)
            .build();
        let reader = TextCtrl::builder(&right)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
            .build();
        msaa_names::set_name(&list, &tr("Articles"));
        msaa_names::set_name(&reader, &tr("Article text"));

        // Rich reader WebView (opt-in, config full_text_rich_view).
        // Created whenever a backend exists but only shown in rich
        // mode. wxdragon PANICS when creation fails, so the backend
        // check is the gate: no WebView2 runtime / WebKit backend
        // keeps the plain-text reader as the only reader (Python's
        // fallback). Link handling mirrors Python's
        // gui/rich_view_links.py (issue #102): main-frame link clicks
        // go to the system browser; embeds load in view. wxDragon
        // exposes neither IsTargetMainFrame nor the navigation
        // action, so the load window (set_page -> Loaded), named
        // sub-frames, and embed-shaped URLs are the discriminators.
        let rich_view: std::rc::Rc<Option<WebView>> =
            std::rc::Rc::new(if WebView::is_backend_available(WebViewBackend::Default) {
                Some(WebView::builder(&right).build())
            } else {
                None
            });
        let rich_loading = std::rc::Rc::new(std::cell::Cell::new(false));
        if let Some(wv) = rich_view.as_ref() {
            wv.show(false);
            let loading = rich_loading.clone();
            wv.on_loaded(move |_| loading.set(false));
            let frame_nav = frame;
            let loading_nav = rich_loading.clone();
            wv.on_navigating(move |data| {
                if loading_nav.get() {
                    return;
                }
                let Some(url) = data.get_url() else { return };
                if !data.get_target().unwrap_or_default().is_empty() {
                    return; // named sub-frame: let it load
                }
                if rich_embed_url(&url) {
                    return; // social/player embeds render in view
                }
                if let Some(safe) = rich_external_url(&url) {
                    data.event.event.veto();
                    open_article_link(&frame_nav, &safe);
                }
            });
            let frame_win = frame;
            wv.on_new_window(move |data| {
                if let Some(url) = data.get_url()
                    && let Some(safe) = rich_external_url(&url)
                {
                    open_article_link(&frame_win, &safe);
                }
            });
        }

        let right_sizer = BoxSizer::builder(Orientation::Vertical).build();
        right_sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 2);
        right_sizer.add(&reader, 2, SizerFlag::Expand | SizerFlag::All, 2);
        if let Some(wv) = rich_view.as_ref() {
            right_sizer.add(wv, 2, SizerFlag::Expand | SizerFlag::All, 2);
        }
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
        {
            // Search field visibility persists (show_search_field,
            // default shown).
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if !cfg.get_bool("show_search_field", true) {
                search_box.show(false);
            }
        }

        let state = Rc::new(RefCell::new(State {
            db: None,
            store_path: db_path(),
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
            refresh_paused_until: None,
            read_filter: 0,
            media_filter: 0,
            sort_by: "date".to_string(),
            sort_ascending: false,
            view_total: -1,
            search_terms: Vec::new(),
            search_title_only: false,
            ctx_mark: None,
            find_query: String::new(),
            pending_play: None,
            pending_queue: None,
            refresh_new: Vec::new(),
            refresh_failed: false,
            search_focused: false,
            expanded_cats: std::collections::HashSet::new(),
            collapsed_cats: std::collections::HashSet::new(),
            cols_sig: String::new(),
            refill_pending: false,
            reader_cache: blindrss_core::reader_cache::ReaderCache::default(),
            reader_generation: 0,
            pending_fulltext: None,
            auto_load: None,
        }));

        // A staged restore (Python backup.py semantics) applies
        // before the database opens; the replaced files are kept
        // as .pre-restore siblings. It swaps the database the app is
        // about to open, which need not be <data_dir>/rss.db.
        let path = db_path();
        let _ = blindrss_core::maintenance::apply_pending_restore(&data_dir(), &path);
        // Open the database and populate (creating it on a fresh
        // install, as Python does).
        {
            let mut st = state.borrow_mut();
            match Db::open_or_create(&path) {
                Ok(db) => {
                    // Hosted isolation: with Local active, the
                    // base database must not carry hosted-cache
                    // rows (older syncs wrote every account into
                    // it). Each hosted account keeps its own
                    // cache file and its server is authoritative,
                    // so the strays are dropped before the tree
                    // loads.
                    {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        if blindrss_core::provider::active_name(&cfg) == "local" {
                            let _ = db.purge_hosted_rows();
                        }
                    }
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
                    // Shared-format decode (Audit 8 G20): a
                    // Python-written `category:` id and an
                    // `unread:`/`read:` filter prefix restore the
                    // category view and the read filter instead of
                    // being treated as a feed id / ignored.
                    let (selected, read_filter) = restored_view_state(last);
                    st.selected_feed = selected;
                    st.read_filter = read_filter;
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
            st.reader_generation = st.reader_generation.wrapping_add(1);
            st.pending_fulltext = None;
            st.selected_feed = filter;
            st.focus_list = false;
            if st.remember_last_feed {
                // Store in the shared Python format (Audit 8
                // G20), wrapping the current read filter the way
                // Python's _commit_tree_selection does.
                let value = remembered_view_value(st.selected_feed.as_deref(), st.read_filter);
                let path = config_path();
                let mut cfg = blindrss_core::config::Config::load(&path).unwrap_or_default();
                if cfg.get_str("last_selected_feed", "") != value {
                    cfg.set_path("last_selected_feed", serde_json::json!(value));
                    let _ = cfg.save(&path);
                }
            }
            // Defer the fill to the drain timer: arrowing
            // through the tree marks it pending repeatedly and
            // only the node he lands on gets filled.
            st.refill_pending = true;
        });

        // Category expansion memory (Python issue #33): record the
        // user's explicit expand/collapse so rebuilds re-apply it
        // instead of resetting to the configured default.
        let state_exp = state.clone();
        tree.on_item_expanded(move |ev| {
            let Some(item) = ev.get_item() else { return };
            if let Some(path) = tree
                .get_custom_data_direct(&item)
                .and_then(|d| d.downcast_ref::<String>().cloned())
                .and_then(|d| d.strip_prefix("cat:").map(|s| s.to_string()))
            {
                let mut st = state_exp.borrow_mut();
                st.collapsed_cats.remove(&path);
                st.expanded_cats.insert(path);
            }
        });
        let state_col = state.clone();
        tree.on_item_collapsed(move |ev| {
            let Some(item) = ev.get_item() else { return };
            if let Some(path) = tree
                .get_custom_data_direct(&item)
                .and_then(|d| d.downcast_ref::<String>().cloned())
                .and_then(|d| d.strip_prefix("cat:").map(|s| s.to_string()))
            {
                let mut st = state_col.borrow_mut();
                st.expanded_cats.remove(&path);
                st.collapsed_cats.insert(path);
            }
        });

        // Selecting an article only PREVIEWS it (Python's
        // on_article_select): no read/opened marking happens
        // here — arrowing across unread rows must leave them
        // unread. Marking belongs to reader focus and to
        // activation (Audit 8 G01).
        let state_list = state.clone();
        let frame_list = frame;
        let rich_view_sel = rich_view.clone();
        let rich_loading_sel = rich_loading.clone();
        let tx_sel = tx.clone();
        list.on_item_selected(move |ev| {
            let idx = ev.get_item_index();
            if idx < 0 {
                return;
            }
            let mut st = state_list.borrow_mut();
            st.reader_generation = st.reader_generation.wrapping_add(1);
            st.pending_fulltext = None;
            st.selected_article = Some(idx as usize);
            st.focus_list = true;
            let Some(mut a) = st.articles.get(idx as usize).cloned() else {
                return;
            };
            // List rows are light (no bodies): resolve this
            // article's body once and cache it back into the row
            // so re-selecting it is free.
            ensure_content(st.db.as_ref(), &mut a);
            if let Some(row) = st.articles.get_mut(idx as usize) {
                row.content = a.content.clone();
            }
            let body = a
                .content
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| a.description.clone())
                .unwrap_or_else(|| "(no stored text)".to_string());
            let raw_body = body.clone();
            // Stored bodies are HTML; the reader shows clean text.
            // Image alt markers follow the per-feed / global
            // show_image_alt setting.
            let keep_alt = {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                show_image_alt_for(&cfg, st.db.as_ref(), &a.feed_id)
            };
            let body = blindrss_core::extract::html_to_text_opts(&body, keep_alt);
            reader.set_value(&format!("{}\n\n{}", a.title, body));
            // Rich reader (opt-in, full_text_rich_view): the WebView
            // shows the cleaned feed body now; the full rich article
            // replaces it when the worker returns (RichHtml in the
            // drain). Any failure keeps the plain-text reader.
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if cfg.get_bool("full_text_rich_view", false) {
                if let Some(wv) = rich_view_sel.as_ref() {
                    let frag = blindrss_core::article_html::rich_article_from_feed(
                        &a.url,
                        &raw_body,
                        &a.title,
                        a.author.as_deref().unwrap_or(""),
                        a.date
                            .as_deref()
                            .unwrap_or("")
                            .split('T')
                            .next()
                            .unwrap_or(""),
                        "",
                        "",
                        &ui_language(),
                    );
                    match frag {
                        Some(frag) => {
                            rich_loading_sel.set(true);
                            wv.set_page(&rich_document(&frag), &a.url);
                            wv.show(true);
                            reader.show(false);
                            frame_list.layout();
                            spawn_rich_fetch(&tx_sel, &a);
                        }
                        None => {
                            wv.show(false);
                            reader.show(true);
                        }
                    }
                }
            } else {
                if let Some(wv) = rich_view_sel.as_ref() {
                    wv.show(false);
                }
                reader.show(true);
            }
            // Automatic full-text load (Audit 8 G02/O07,
            // Python on_article_selected): with translation
            // on, the pane must end up translated without a
            // focus action; on macOS VoiceOver never fires
            // the reader focus event; forum threads and
            // YouTube videos syndicate only a stub. Debounced
            // and generation-guarded, so fast arrowing is
            // safe; every other article waits for reader
            // focus (the SET_FOCUS binding below).
            // Selection still marks nothing (Audit 8 G01,
            // Task 11): Task 12's mark-on-select block was
            // dropped at integration — marking belongs to
            // reader focus and activation.
            if auto_fulltext_on_select(
                blindrss_core::translate::settings_from_config(&cfg).is_some(),
                cfg!(target_os = "macos"),
                blindrss_core::forum_sources::is_forum_thread_host(&a.url),
                blindrss_core::ytfull::is_youtube_video_url(&a.url),
            ) {
                schedule_auto_fulltext(&mut st, false);
            }
        });

        // Reader focus marks the selected article read +
        // opened (Python's on_content_focus; Audit 8 G01).
        // The rich WebView reader carries the same trigger
        // (Python's _on_rich_view_focus marks identically).
        let state_rf = state.clone();
        let frame_rf = frame;
        reader.on_set_focus(move |_| {
            let marked = {
                let mut st = state_rf.borrow_mut();
                mark_selected_read_opened(&mut st, &frame_rf)
            };
            if let Some(a) = marked {
                propagate_read_state(&a, true);
            }
        });
        if let Some(wv) = rich_view.as_ref() {
            let state_wf = state.clone();
            let frame_wf = frame;
            wv.on_set_focus(move |_| {
                let marked = {
                    let mut st = state_wf.borrow_mut();
                    mark_selected_read_opened(&mut st, &frame_wf)
                };
                if let Some(a) = marked {
                    propagate_read_state(&a, true);
                }
            });
        }

        // Refresh: worker threads fetch + parse; results return over a
        // channel; a timer drains them on the main thread, which owns
        // the database connection.
        /// Per-host connection gate (config
        /// per_host_max_connections): a counting semaphore per
        /// host so one host never takes more than its share of
        /// the refresh workers.
        struct HostGate {
            state: std::sync::Mutex<usize>,
            cond: std::sync::Condvar,
            max: usize,
        }
        impl HostGate {
            fn acquire(&self) {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                while *guard >= self.max {
                    guard = self.cond.wait(guard).unwrap_or_else(|p| p.into_inner());
                }
                *guard += 1;
            }
            fn release(&self) {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                *guard = guard.saturating_sub(1);
                self.cond.notify_one();
            }
        }

        fn spawn_refresh(
            feeds: Vec<Feed>,
            tx: Sender<RefreshMsg>,
            cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
            stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
            force_full: bool,
        ) {
            // Refresh engine knobs (Audit 3): worker pool size,
            // per-host cap, retry attempts — read once per run.
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let workers = cfg.get_i64("max_concurrent_refreshes", 16).clamp(1, 64) as usize;
            let per_host = cfg.get_i64("per_host_max_connections", 4).clamp(1, 64) as usize;
            let attempts = cfg.get_i64("feed_retry_attempts", 1).clamp(1, 10) as u32;
            // Store knobs for the worker refresh: the global delete
            // behavior and the shared retention boundary (Python
            // computes both once per refresh run).
            let global_behavior = cfg.get_str("delete_behavior", "deleted").to_string();
            let retention_cutoff = blindrss_core::retention::retention_cutoff_date(
                blindrss_core::retention::retention_days(
                    cfg.get_str("article_retention", "unlimited"),
                ),
            );
            let total = feeds.len();
            let path = db_path();
            if total == 0 {
                let _ = tx.send(RefreshMsg::AllDone(path));
                return;
            }
            let queue = std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::from(feeds),
            ));
            let gates = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
                String,
                std::sync::Arc<HostGate>,
            >::new()));
            let workers_left =
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(workers.min(total)));
            for _ in 0..workers.min(total) {
                let queue = queue.clone();
                let gates = gates.clone();
                let tx = tx.clone();
                let cancel = cancel.clone();
                let stopped = stopped.clone();
                let workers_left = workers_left.clone();
                let path = path.clone();
                let global_behavior = global_behavior.clone();
                let retention_cutoff = retention_cutoff.clone();
                std::thread::spawn(move || {
                    let database = Db::open(&path);
                    loop {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        let feed = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
                        let Some(feed) = feed else { break };
                        let host = feed.url.split('/').nth(2).unwrap_or("").to_string();
                        let gate = gates
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .entry(host)
                            .or_insert_with(|| {
                                std::sync::Arc::new(HostGate {
                                    state: std::sync::Mutex::new(0),
                                    cond: std::sync::Condvar::new(),
                                    max: per_host,
                                })
                            })
                            .clone();
                        gate.acquire();
                        // Unified worker refresh (Audit 8 D16/D22/F01):
                        // source dispatch, fetch, store and media
                        // attachment all run here, on this worker's
                        // own DB connection; only the committed
                        // outcome is sent to the drain.
                        let work = || match &database {
                            Ok(db) => blindrss_core::refresh::refresh_feed_with_options(
                                db,
                                &feed,
                                &global_behavior,
                                retention_cutoff.as_deref(),
                                force_full,
                                &cancel,
                            )
                            .map(|w| w.outcome),
                            Err(error) => Err(format!("Database: {error}")),
                        };
                        let mut result = work();
                        for _ in 1..attempts {
                            if result.is_ok() || cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            result = work();
                        }
                        gate.release();
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) && result.is_err() {
                            // A stop, not a failure: record nothing
                            // and report nothing (AllDone announces
                            // the stop, as before).
                            stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        if let Ok(db) = &database {
                            match &result {
                                Ok(_) => {
                                    let _ = db.clear_feed_error(&feed.id);
                                }
                                Err(e) => {
                                    let _ = db.record_feed_error(&feed.id, e);
                                }
                            }
                        }
                        let _ = tx.send(RefreshMsg::FeedDone(path.clone(), feed, result));
                    }
                    if workers_left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                        let _ = tx.send(RefreshMsg::AllDone(path));
                    }
                });
            }
        }
        // Single-instance listener: a second copy's activation
        // request raises this window (via the drain).
        {
            let tx_act = tx.clone();
            blindrss_core::single_instance::serve(move || {
                let _ = tx_act.send(RefreshMsg::ActivateWindow);
            });
        }
        // Global show/hide hotkey (Windows only): subclass the
        // frame for WM_HOTKEY and register the configured combo.
        // A startup failure (combo taken) leaves config alone —
        // whoever holds it may release it later (Python semantics).
        #[cfg(target_os = "windows")]
        {
            let hwnd = frame.get_handle() as isize;
            hotkey::install(hwnd, tx.clone());
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let accel = cfg
                .get_str("global_show_hide_hotkey", "Ctrl+Alt+B")
                .to_string();
            let _ = hotkey::apply(hwnd, &accel);
        }
        // Startup dependency check (Python mainframe parity):
        // off the UI thread because detection shells out to
        // --version probes; the drain shows the prompt.
        {
            let tx_dep = tx.clone();
            std::thread::spawn(move || {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if !cfg.get_bool("prompt_missing_dependencies_on_startup", true) {
                    return;
                }
                let missing: Vec<String> = blindrss_core::dependency::check_tools()
                    .into_iter()
                    .filter(|t| t.path.is_none())
                    .map(|t| t.name)
                    .collect();
                if !missing.is_empty() {
                    let _ = tx_dep.send(RefreshMsg::DepsMissing(missing));
                }
            });
            // Nightly yt-dlp maintenance (Audit 8 O06; Python
            // _ensure_yt_dlp_cli): an EXISTING yt-dlp is kept on
            // the nightly channel — throttled to one successful
            // check per 24 h inside core, failure-safe (a failed
            // update keeps the working copy and does not burn the
            // throttle). Off the UI thread; result only logged.
            std::thread::spawn(move || {
                if let Some(report) = blindrss_core::dependency::maintain_ytdlp(&data_dir()) {
                    log_line(&format!(
                        "yt-dlp maintenance: {:?} at {} (version {})",
                        report.action,
                        report.path.display(),
                        report.version.as_deref().unwrap_or("unknown")
                    ));
                }
            });
        }
        let rx = Rc::new(RefCell::new(rx));
        let state_btn = state.clone();
        let frame_btn = frame;
        let tx_refresh = tx.clone();
        let refresh_force = Rc::new(std::cell::Cell::new(true));
        let force_next = refresh_force.clone();
        let act_refresh: Rc<dyn Fn()> = Rc::new(move || {
            let force_full = force_next.replace(true);
            // Active-provider routing (PARITY 5A): when a hosted
            // provider is the backend, Refresh syncs that
            // provider instead of fetching local feeds.
            {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if blindrss_core::provider::active_name(&cfg) != "local" {
                    {
                        let mut st = state_btn.borrow_mut();
                        if st.refreshing {
                            return;
                        }
                        st.refresh_paused_until = None;
                        st.refreshing = true;
                    }
                    frame_btn.set_status_text("Syncing provider...", 0);
                    announce("start_update", "Refresh Feeds");
                    let tx = tx_refresh.clone();
                    let path = blindrss_core::provider::active_db_path(&cfg, &base_db_path());
                    std::thread::spawn(move || {
                        let provider = blindrss_core::provider::get_provider(&cfg, path.clone());
                        let tx_late = tx.clone();
                        let result = provider.refresh_with_updates(
                            force_full,
                            Box::new(move |result| {
                                let _ = tx_late.send(RefreshMsg::ProviderAction(result));
                            }),
                        );
                        // Podcast archive (gap 6): after a
                        // hosted sync, queue archive scans for
                        // the due feeds (Python's
                        // _queue_podcast_archive_scans).
                        if result.is_ok() {
                            let settings =
                                blindrss_core::archive_store::ArchiveSettings::from_config(&cfg);
                            if settings.enabled {
                                let scope = blindrss_core::archive_store::scope_for_config(&cfg);
                                if let Ok(db) = Db::open(&path) {
                                    let pairs: Vec<(String, String)> = db
                                        .feeds()
                                        .unwrap_or_default()
                                        .iter()
                                        .map(|f| {
                                            (
                                                f.id.clone(),
                                                blindrss_core::archive_store::podcast_source_url(
                                                    &f.id, &f.url,
                                                ),
                                            )
                                        })
                                        .collect();
                                    blindrss_core::archive_store::queue_hosted_scans(
                                        &path, &scope, &settings, &pairs,
                                    );
                                }
                            }
                        }
                        let _ = tx.send(RefreshMsg::ProviderAction(result));
                    });
                    return;
                }
            }
            let (feeds, cancel, stopped) = {
                let mut st = state_btn.borrow_mut();
                if st.refreshing {
                    return;
                }
                st.refresh_paused_until = None;
                st.refreshing = true;
                st.refresh_new.clear();
                st.refresh_failed = false;
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
            spawn_refresh(feeds, tx_refresh.clone(), cancel, stopped, force_full);
        });
        {
            let a = act_refresh.clone();
            refresh_btn.on_click(move |_| a());
        }
        *tray_refresh_slot.borrow_mut() = Some(act_refresh.clone());

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
                        st.refresh_paused_until = None;
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
                // Hosted feeds refresh through their provider's
                // per-feed path, not by fetching the site.
                if feed.id.starts_with("feed/") || feed.id.starts_with("miniflux:") {
                    let tx = tx_rf.clone();
                    let feed2 = feed.clone();
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let providers = cfg.raw().get("providers").cloned();
                        let get = |obj: &serde_json::Value, key: &str| -> String {
                            obj.get(key)
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        };
                        let result = (|| -> Result<usize, String> {
                            let db = Db::open(&db_path()).map_err(|e| format!("{e}"))?;
                            let Some(providers) = providers else {
                                return Err("no providers configured".to_string());
                            };
                            if let Some(mf) = feed2.id.strip_prefix("miniflux:") {
                                let section = providers.get("miniflux");
                                let (url, key) = (
                                    section.map(|s| get(s, "url")).unwrap_or_default(),
                                    section.map(|s| get(s, "api_key")).unwrap_or_default(),
                                );
                                let id: i64 = mf.parse().map_err(|_| "bad feed id")?;
                                return blindrss_core::providers::MinifluxClient::new(&url, &key)
                                    .with_refresh_config(&cfg)
                                    .refresh_feed(&db, id);
                            }
                            use blindrss_core::providers::{GReaderClient, GReaderService};
                            for (key, service) in [
                                ("theoldreader", GReaderService::TheOldReader),
                                ("bazqux", GReaderService::BazQux),
                            ] {
                                if let Some(section) = providers.get(key) {
                                    let (email, password) =
                                        (get(section, "email"), get(section, "password"));
                                    if !email.is_empty() && !password.is_empty() {
                                        return GReaderClient::new(service, &email, &password)
                                            .refresh_feed(&db, &feed2.id);
                                    }
                                }
                            }
                            if let Some(section) = providers.get("inoreader") {
                                let token = get(section, "token");
                                if !token.is_empty() {
                                    let cfg = blindrss_core::config::Config::load(&config_path())
                                        .unwrap_or_default();
                                    return GReaderClient::with_oauth(
                                        GReaderService::Inoreader,
                                        &token,
                                    )
                                    .with_article_page_size(
                                        blindrss_core::providers::page_size_from_config(&cfg),
                                    )
                                    .refresh_feed(&db, &feed2.id);
                                }
                            }
                            Err("hosted provider credentials not set".to_string())
                        })();
                        let _ = tx.send(RefreshMsg::ProviderAction(
                            result.map(|n| format!("{}: {n} new article(s)", feed2.title)),
                        ));
                    });
                    return;
                }
                let (cancel, stopped) = {
                    let st = state_rf.borrow();
                    st.cancel_refresh
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    st.refresh_stopped
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    (st.cancel_refresh.clone(), st.refresh_stopped.clone())
                };
                spawn_refresh(vec![feed], tx_rf.clone(), cancel, stopped, true);
            })
        };

        // Article actions: delete (tombstoned), favorite, mark all
        // read, restore-deleted dialog, and search.
        let state_del = state.clone();
        let frame_del = frame;
        let act_delete: Rc<dyn Fn()> = Rc::new(move || {
            let articles: Vec<Article> = {
                let st = state_del.borrow();
                selected_article_indices(&list, &st)
                    .iter()
                    .map(|&i| st.articles[i].clone())
                    .collect()
            };
            if articles.is_empty() {
                frame_del.set_status_text("Select an article first", 0);
                return;
            }
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if cfg.get_bool("confirm_article_delete", true) {
                let prompt = if articles.len() == 1 {
                    format!("Delete '{}'?", articles[0].title)
                } else {
                    format!("Delete {} selected articles?", articles.len())
                };
                let md = MessageDialog::builder(&frame_del, &prompt, "Delete Article")
                    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                    .build();
                let answer = md.show_modal();
                md.destroy();
                restore_focus(&state_del.borrow(), &tree, &list);
                if answer != ID_YES {
                    return;
                }
            }
            let mut st = state_del.borrow_mut();
            let mut done = 0;
            let mut failure = None;
            if let Some(db) = st.db.as_ref() {
                for a in &articles {
                    // Delete behaviour: one shared resolution —
                    // the feed's override wins over the global
                    // setting, parsed exactly like Python's
                    // parse_delete_behavior (Audit 8 D12).
                    let behavior = db.effective_delete_behavior(
                        &a.feed_id,
                        cfg.get_str("delete_behavior", "deleted"),
                    );
                    let result =
                        if a.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
                            db.delete_recommendation(&a.id).map(|_| ())
                        } else {
                            db.delete_article_with_behavior(&a.id, &behavior)
                                .map(|_| ())
                        };
                    match result {
                        Ok(_) => done += 1,
                        Err(e) => {
                            failure = Some(e.to_string());
                            break;
                        }
                    }
                }
            }
            st.selected_article = None;
            refill_articles(&mut st, &list);
            frame_del.set_status_text(
                &match failure {
                    Some(e) => format!("Processed {done} article(s); delete failed: {e}"),
                    None => format!("Processed {done} selected article(s)"),
                },
                0,
            );
        });
        {
            let a = act_delete.clone();
            delete_btn.on_click(move |_| a());
        }

        let state_fav = state.clone();
        let frame_fav = frame;
        let act_fav: Rc<dyn Fn()> = Rc::new(move || {
            let mut st = state_fav.borrow_mut();
            let indices = selected_article_indices(&list, &st);
            if indices.is_empty() {
                frame_fav.set_status_text("Select an article first", 0);
                return;
            }
            let articles: Vec<Article> = indices.iter().map(|&i| st.articles[i].clone()).collect();
            let new_value = !articles.iter().all(|a| a.is_favorite);
            for (&i, article) in indices.iter().zip(&articles) {
                if let Some(db) = &st.db {
                    mark_article_favorite(db, article, new_value);
                }
                st.articles[i].is_favorite = new_value;
            }
            // Hosted backend: favorites are the server's
            // starred/bookmarked state — propagate to the
            // active provider only, beside the local write
            // above (propagate_read_state's pattern).
            // Recommendations are local-only.
            for article in &articles {
                propagate_favorite_state(article, new_value);
            }
            refill_articles(&mut st, &list);
            frame_fav.set_status_text(
                if new_value {
                    "Added to favorites"
                } else {
                    "Removed from favorites"
                },
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
        let tx_ma = tx.clone();
        // Mark All Read (Audit 8 G05): ONE operation
        // (run_mark_all over an explicit scope) behind distinct
        // wrappers, like Python — the main command (File menu,
        // button, feeds.mark_all_read shortcut) is GLOBAL and
        // confirms with the global prompt; the tree context
        // command is scoped to the node its menu was opened on
        // and confirms with that node's prompt. A cancelled
        // confirmation changes no state at all: the dialog runs
        // before anything is borrowed, marked or announced.
        let act_markall: Rc<dyn Fn()> = Rc::new(move || {
            if !confirm_mark_all(&frame_ma, &mark_all_prompt(None, None)) {
                return;
            }
            run_mark_all(&state_ma, &frame_ma, &tx_ma, None);
        });
        let state_mc = state.clone();
        let frame_mc = frame;
        let tx_mc = tx.clone();
        let act_markall_ctx: Rc<dyn Fn()> = Rc::new(move || {
            let Some((view, prompt)) = state_mc.borrow_mut().ctx_mark.take() else {
                return;
            };
            if !confirm_mark_all(&frame_mc, &prompt) {
                return;
            }
            run_mark_all(&state_mc, &frame_mc, &tx_mc, view);
        });
        {
            let a = act_markall.clone();
            markall_btn.on_click(move |_| a());
        }

        let state_s = state.clone();
        let frame_s = frame;
        let act_search: Rc<dyn Fn()> = Rc::new(move || {
            // In-view search (Audit 8 G04): the terms become
            // state and the ordinary refill pipeline applies
            // them to the CURRENT view — scope, read/media
            // filters and sort preserved, and the results page
            // like the view itself. (The old code ran one
            // global phrase search capped at 300 and replaced
            // the list outright.) An empty box clears the
            // filter, Python's on_search_enter behaviour.
            let query = search_box.get_value();
            let mut st = state_s.borrow_mut();
            st.search_terms = blindrss_core::db::search_terms(&query);
            st.search_title_only = blindrss_core::config::Config::load(&config_path())
                .map(|c| c.get_str("search_mode", "title_content") == "title_only")
                .unwrap_or(false);
            let searching = !st.search_terms.is_empty();
            refill_articles(&mut st, &list);
            if searching {
                let n = if st.view_total >= 0 {
                    st.view_total
                } else {
                    st.articles.len() as i64
                };
                frame_s.set_status_text(&format!("{n} matches"), 0);
            }
        });
        {
            let a = act_search.clone();
            search_btn.on_click(move |_| a());
            // Enter in the field runs the search too (Python's
            // on_search_enter on EVT_TEXT_ENTER).
            let a = act_search.clone();
            search_box.on_enter_pressed(move |_| a());
        }

        let state_deleted = state.clone();
        let frame_deleted = frame;
        let act_deleted: Rc<dyn Fn()> = Rc::new(move || {
            // Deleted Articles (Python's Deleted view, Audit 8
            // G07): normal article columns, search filtering,
            // multi-restore and confirmed permanent purge. A row
            // leaves the list only when its operation succeeded;
            // failures stay put and are reported.
            let (deleted, feed_titles_owned) = {
                let st = state_deleted.borrow();
                let Some(db) = st.db.as_ref() else { return };
                let rows = db.deleted_articles().unwrap_or_default();
                let titles: std::collections::HashMap<String, String> = st
                    .feeds
                    .iter()
                    .map(|f| (f.id.clone(), f.title.clone()))
                    .collect();
                (rows, titles)
            };
            if deleted.is_empty() {
                frame_deleted.set_status_text("No deleted articles", 0);
                return;
            }
            let dlg = Dialog::builder(&frame_deleted, "Deleted Articles").build();
            dlg.set_size(Size::new(820, 480));
            let search_label = StaticText::builder(&dlg).with_label("Search:").build();
            let search_box = TextCtrl::builder(&dlg).build();
            let lc = ListCtrl::builder(&dlg)
                .with_style(ListCtrlStyle::Report)
                .build();
            setup_article_columns(&lc);
            let status_lbl = StaticText::builder(&dlg).with_label("").build();
            let open_btn = Button::builder(&dlg).with_label("Open").build();
            let restore_btn = Button::builder(&dlg).with_label("Restore Selected").build();
            let purge_btn = Button::builder(&dlg)
                .with_label("Delete Permanently...")
                .build();
            let close_btn = Button::builder(&dlg).with_label("Close").build();
            let ds = BoxSizer::builder(Orientation::Vertical).build();
            let srow = BoxSizer::builder(Orientation::Horizontal).build();
            srow.add(&search_label, 0, SizerFlag::All, 4);
            srow.add(&search_box, 1, SizerFlag::Expand | SizerFlag::All, 4);
            ds.add_sizer(&srow, 0, SizerFlag::Expand | SizerFlag::All, 0);
            ds.add(&lc, 1, SizerFlag::Expand | SizerFlag::All, 4);
            ds.add(&status_lbl, 0, SizerFlag::Expand | SizerFlag::All, 4);
            let drow = BoxSizer::builder(Orientation::Horizontal).build();
            drow.add(&open_btn, 0, SizerFlag::All, 4);
            drow.add(&restore_btn, 0, SizerFlag::All, 4);
            drow.add(&purge_btn, 0, SizerFlag::All, 4);
            drow.add(&close_btn, 0, SizerFlag::All, 4);
            ds.add_sizer(&drow, 0, SizerFlag::All, 0);
            dlg.set_sizer(ds, true);

            let all_rows = Rc::new(std::cell::RefCell::new(deleted));
            let shown = Rc::new(std::cell::RefCell::new(all_rows.borrow().clone()));
            let feed_titles_owned = Rc::new(feed_titles_owned);
            // (Re)fill the list from the shown rows, honouring
            // the configured article columns.
            let fill: Rc<dyn Fn()> = {
                let shown_f = shown.clone();
                let titles_f = feed_titles_owned.clone();
                Rc::new(move || {
                    let cfg =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    let layout = blindrss_core::article_columns::layout_from_config(&cfg);
                    let visible = blindrss_core::article_columns::visible_columns(&layout);
                    let titles: std::collections::HashMap<&str, &str> = titles_f
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect();
                    lc.freeze();
                    lc.delete_all_items();
                    for (i, a) in shown_f.borrow().iter().enumerate() {
                        let mut row = a.clone();
                        row.title = deleted_display_title(a);
                        lc.insert_item(i as i64, &row.title, None);
                        for (col_idx, col) in visible.iter().enumerate().skip(1) {
                            let cell = article_cell(&row, &col.key, &titles);
                            lc.set_item_text_by_column(i as i64, col_idx as i32, &cell);
                        }
                    }
                    lc.thaw();
                })
            };
            fill();
            // Search: every term must appear in the row's title,
            // feed, author or URL (Python's view search shape).
            // One closure, reused after restore/purge passes.
            let refilter: Rc<dyn Fn()> = {
                let all_s = all_rows.clone();
                let shown_s = shown.clone();
                let titles_s = feed_titles_owned.clone();
                let fill_s = fill.clone();
                Rc::new(move || {
                    let query = search_box.get_value().to_lowercase();
                    let terms: Vec<&str> = query.split_whitespace().collect();
                    let filtered: Vec<Article> = all_s
                        .borrow()
                        .iter()
                        .filter(|a| {
                            if terms.is_empty() {
                                return true;
                            }
                            let hay = format!(
                                "{} {} {} {}",
                                a.title.to_lowercase(),
                                titles_s
                                    .get(&a.feed_id)
                                    .map(|s| s.to_lowercase())
                                    .unwrap_or_default(),
                                a.author.clone().unwrap_or_default().to_lowercase(),
                                a.url.to_lowercase()
                            );
                            terms.iter().all(|t| hay.contains(t))
                        })
                        .cloned()
                        .collect();
                    *shown_s.borrow_mut() = filtered;
                    fill_s();
                })
            };
            {
                let r = refilter.clone();
                search_box.on_text_updated(move |_| r());
            }
            let selected_rows: Rc<dyn Fn() -> Vec<Article>> = {
                let shown_sel = shown.clone();
                Rc::new(move || {
                    let mut out = Vec::new();
                    let mut index = lc.get_first_selected_item();
                    while index >= 0 {
                        if let Some(a) = shown_sel.borrow().get(index as usize) {
                            out.push(a.clone());
                        }
                        index = lc.get_next_item(
                            index as i64,
                            ListNextItemFlag::All,
                            ListItemState::Selected,
                        );
                    }
                    out
                })
            };
            // One pass of restore or purge over the selection;
            // only succeeded rows leave the dialog (and the
            // failure count is announced, never swallowed).
            let apply_op: Rc<dyn Fn(bool)> = {
                let all_o = all_rows.clone();
                let state_o = state_deleted.clone();
                let selected_o = selected_rows.clone();
                let refilter_o = refilter.clone();
                Rc::new(move |purge: bool| {
                    let targets = selected_o();
                    if targets.is_empty() {
                        status_lbl.set_label("Select one or more articles first");
                        return;
                    }
                    let mut succeeded: Vec<(String, String)> = Vec::new();
                    {
                        let st = state_o.borrow();
                        if let Some(db) = st.db.as_ref() {
                            for a in &targets {
                                let ok = if purge {
                                    db.purge_deleted_article(&a.feed_id, &a.id)
                                } else {
                                    db.restore_deleted_article(&a.feed_id, &a.id)
                                };
                                if matches!(ok, Ok(true)) {
                                    succeeded.push((a.feed_id.clone(), a.id.clone()));
                                }
                            }
                        }
                    }
                    let failed = targets.len() - succeeded.len();
                    let done = succeeded.len();
                    *all_o.borrow_mut() =
                        retain_failed_deleted_rows(all_o.borrow().clone(), &succeeded);
                    // Re-apply the current search filter; failed
                    // rows are still in all_rows, so they reappear.
                    refilter_o();
                    let verb = if purge {
                        "Permanently deleted"
                    } else {
                        "Restored"
                    };
                    let note = if failed > 0 {
                        format!("{verb} {done} article(s); {failed} failed and remain in the list")
                    } else {
                        format!("{verb} {done} article(s)")
                    };
                    status_lbl.set_label(&note);
                    frame_deleted.set_status_text(&note, 0);
                    if done > 0 {
                        let mut st = state_o.borrow_mut();
                        refill_articles(&mut st, &list);
                    }
                })
            };
            {
                let a = apply_op.clone();
                restore_btn.on_click(move |_| a(false));
            }
            {
                let a = apply_op.clone();
                let selected_p = selected_rows.clone();
                purge_btn.on_click(move |_| {
                    let n = selected_p().len();
                    if n == 0 {
                        status_lbl.set_label("Select one or more articles first");
                        return;
                    }
                    let prompt = if n == 1 {
                        "Permanently delete this article? It cannot be restored.".to_string()
                    } else {
                        format!("Permanently delete {n} articles? They cannot be restored.")
                    };
                    let md = MessageDialog::builder(&dlg, &prompt, "Confirm Permanent Delete")
                        .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconWarning)
                        .build();
                    let answer = md.show_modal();
                    md.destroy();
                    if answer == ID_YES {
                        a(true);
                    }
                });
            }
            let open_selected: Rc<dyn Fn()> = {
                let selected_q = selected_rows.clone();
                Rc::new(move || {
                    let targets = selected_q();
                    let Some(a) = targets.first() else {
                        status_lbl.set_label("Select an article first");
                        return;
                    };
                    let body = a
                        .content
                        .clone()
                        .filter(|c| !c.trim().is_empty())
                        .or_else(|| a.description.clone())
                        .unwrap_or_default();
                    crate::article_window::open(&deleted_display_title(a), &body, Some(&a.url));
                })
            };
            {
                let o = open_selected.clone();
                open_btn.on_click(move |_| o());
            }
            {
                let o = open_selected.clone();
                lc.on_item_activated(move |_| o());
            }
            let dlg_close = dlg;
            close_btn.on_click(move |_| {
                dlg_close.end_modal(ID_OK);
            });
            dlg.show_modal();
            dlg.destroy();
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
            // YouTube account wrapper (factory.py applies it
            // over every provider): sync its subscriptions as
            // feeds whenever the account is signed in.
            if !cfg.get_str("youtube_account_refresh_token", "").is_empty() {
                let tx = tx_sync.clone();
                let path = db_path();
                let cpath = cfg_path.clone();
                std::thread::spawn(move || {
                    let result = Db::open(&path)
                        .map_err(|e| format!("{e}"))
                        .and_then(|db| {
                            let mut cfg2 =
                                blindrss_core::config::Config::load(&cpath).unwrap_or_default();
                            let added = blindrss_core::youtube_account::sync_account(
                                &mut cfg2, &cpath, &db,
                            )?;
                            // Recommendations snapshot (the
                            // provider wrapper's other half);
                            // its failure does not fail the sync.
                            let token = cfg2
                                .get_str("youtube_account_refresh_token", "")
                                .to_string();
                            let client_id = {
                                let v = cfg2.get_str("youtube_account_client_id", "");
                                if v.is_empty() {
                                    blindrss_core::youtube_account::CLIENT_ID
                                } else {
                                    v
                                }
                                .to_string()
                            };
                            let secret =
                                cfg2.get_str("youtube_account_client_secret", "").to_string();
                            let recs = blindrss_core::youtube_account::list_recommendations(
                                &client_id, &secret, &token,
                            )
                            .and_then(|videos| {
                                let account =
                                    blindrss_core::youtube_account::account_key(&token);
                                db.save_recommendations(&account, &videos)
                                    .map_err(|e| format!("{e}"))?;
                                Ok(videos.len())
                            });
                            Ok(match recs {
                                Ok(n) => format!(
                                    "YouTube account: {added} new channel feed(s), {n} recommendations"
                                ),
                                Err(e) => format!(
                                    "YouTube account: {added} new channel feed(s) (recommendations: {e})"
                                ),
                            })
                        });
                    let _ = tx.send(RefreshMsg::ProviderAction(result));
                });
            }
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
                // Hosted isolation: this account's sync writes
                // only its own cache file, never the active
                // provider's database.
                let path =
                    blindrss_core::provider::db_path_for_provider(&cfg, &base_db_path(), key);
                std::thread::spawn(move || {
                    let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                        blindrss_core::providers::GReaderClient::new(service, &email, &password)
                            .sync(&db, 100)
                    });
                    let _ = tx.send(RefreshMsg::ProviderDone(key.to_string(), result));
                });
            }
            if let Some(_section) = providers.get("inoreader") {
                // The background sync runs the same OAuth
                // refresh lifecycle as the active provider
                // (Audit 8 D07): a stale token is refreshed
                // first and the rotation persisted; a revoked
                // grant is cleared and skipped, a transient
                // failure skips this round with the stored
                // tokens untouched.
                let mut auth = blindrss_core::oauth::InoreaderAuth::from_config(&cfg);
                let mut usable = !auth.access_token.is_empty() || !auth.refresh_token.is_empty();
                if usable && auth.has_app_credentials() {
                    match auth.ensure_fresh_now() {
                        Ok(blindrss_core::oauth::TokenAction::Fresh) => {}
                        Ok(action) => {
                            let mut cfg2 = cfg.clone();
                            auth.apply_to_config(&mut cfg2);
                            let _ = cfg2.save_to_source();
                            if action == blindrss_core::oauth::TokenAction::Revoked {
                                usable = false;
                            }
                        }
                        Err(_) => usable = false,
                    }
                }
                if usable && !auth.access_token.is_empty() {
                    started += 1;
                    let tx = tx_sync.clone();
                    let path = blindrss_core::provider::db_path_for_provider(
                        &cfg,
                        &base_db_path(),
                        "inoreader",
                    );
                    let page_size = blindrss_core::providers::page_size_from_config(&cfg);
                    let (token, app_id, app_key) = (
                        auth.access_token.clone(),
                        auth.app_id.clone(),
                        auth.app_key.clone(),
                    );
                    std::thread::spawn(move || {
                        let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                            blindrss_core::providers::GReaderClient::with_oauth(
                                blindrss_core::providers::GReaderService::Inoreader,
                                &token,
                            )
                            .with_app_credentials(&app_id, &app_key)
                            .with_article_page_size(page_size)
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
                    let path = blindrss_core::provider::db_path_for_provider(
                        &cfg,
                        &base_db_path(),
                        "miniflux",
                    );
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

        // View History: the selected article's recorded versions.
        let state_hist = state.clone();
        let frame_hist = frame;
        let tree_hist = tree;
        let list_hist = list;
        let act_history: Rc<dyn Fn()> = Rc::new(move || {
            {
                let st = state_hist.borrow();
                let Some(article) = st
                    .selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
                else {
                    frame_hist.set_status_text("No article selected", 0);
                    return;
                };
                let Some(db) = st.db.as_ref() else { return };
                if db.count_article_versions(&article.id).unwrap_or(0) == 0 {
                    frame_hist.set_status_text("No change history for this article", 0);
                    return;
                }
                dialogs::show_article_history(&frame_hist, db, &article);
            }
            restore_focus(&state_hist.borrow(), &tree_hist, &list_hist);
        });

        // Read Full Text: fetch the selected article's page on a
        // worker thread; extraction runs on the main thread when the
        // HTML lands (feed body is the fallback, per the cascade).
        let state_ft = state.clone();
        let frame_ft = frame;
        let tx_ft = tx.clone();
        let rich_view_ft = rich_view.clone();
        let act_fulltext: Rc<dyn Fn()> = Rc::new(move || {
            let mut st = state_ft.borrow_mut();
            let Some(mut article) = st
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
            // The stored body is the extraction fallback and
            // feeds the reader cache key: resolve it (list rows
            // are light) before either is computed.
            ensure_content(st.db.as_ref(), &mut article);
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let keep_alt = show_image_alt_for(&cfg, st.db.as_ref(), &article.feed_id);
            let cache_key = reader_cache_key(&article, keep_alt, &translation_variant(&cfg));
            if let Some((text, source)) = st
                .reader_cache
                .get(&cache_key, cfg.get_bool("cache_full_text", false))
            {
                st.reader_generation = st.reader_generation.wrapping_add(1);
                st.pending_fulltext = None;
                reader.set_value(&format!("{}\n\n{}", article.title, text));
                frame_ft.set_status_text(&format!("Full text ({source}, cached)"), 0);
                if cfg.get_bool("full_text_rich_view", false) && rich_view_ft.as_ref().is_some() {
                    spawn_rich_fetch(&tx_ft, &article);
                }
                return;
            }
            if st
                .pending_fulltext
                .as_ref()
                .is_some_and(|(generation, key)| {
                    *generation == st.reader_generation && key == &cache_key
                })
            {
                return;
            }
            st.reader_generation = st.reader_generation.wrapping_add(1);
            st.pending_fulltext = Some((st.reader_generation, cache_key.clone()));
            let request = blindrss_core::reader_cache::ReaderRequest {
                key: cache_key,
                article_id: article.id.clone(),
                url: article.url.clone(),
                generation: st.reader_generation,
            };
            frame_ft.set_status_text("Fetching full text...", 0);
            let tx = tx_ft.clone();
            let url = article.url.clone();
            let article_feed = article.feed_id.clone();
            let article_content = article.content.clone();
            let article_id = article.id.clone();
            std::thread::spawn(move || {
                // YouTube articles use the dedicated full-text path
                // (description + chapters + transcript), like Python.
                if url.contains("youtube.com") || url.contains("youtu.be") {
                    let result = blindrss_core::download::youtube_full_text_in_language(
                        &url,
                        &ui_language(),
                    )
                    .map(auto_translate_text);
                    let _ = tx.send(RefreshMsg::YoutubeText(request, result));
                } else {
                    let jar = blindrss_core::cookies::load_jar_beside(&config_path());
                    let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
                    // Per-feed encoding override (feeds.feed_settings).
                    let encoding = Db::open(&db_path())
                        .ok()
                        .and_then(|db| db.feed_settings(&article_feed).ok())
                        .and_then(|s| {
                            s.get("encoding")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    let (outcome, first_html) = blindrss_core::extract::fetch_full_text_opts(
                        &url,
                        &header,
                        article_content.as_deref(),
                        &encoding,
                        keep_alt,
                    );
                    // Full-text arbitration (PARITY 7.6): when
                    // the active provider is Miniflux, its
                    // server-side fetch-content competes with
                    // the local cascade; the more complete
                    // result wins.
                    let outcome = arbitrate_full_text(outcome, &article_id);
                    // Automatic translation happens here, on
                    // the worker, before caching — Python's
                    // pipeline translates the rendered text
                    // inside its full-text worker (Audit 8 O07).
                    let outcome = match outcome {
                        blindrss_core::extract::ExtractOutcome::Article {
                            title,
                            text,
                            source,
                        } => blindrss_core::extract::ExtractOutcome::Article {
                            title,
                            text: auto_translate_text(text),
                            source,
                        },
                        other => other,
                    };
                    let _ = tx.send(RefreshMsg::FullText(request, outcome, first_html));
                }
            });
            // Rich mode: the WebView gets its own full article. The
            // text result above still lands in the hidden text
            // reader, so toggling modes stays truthful.
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if cfg.get_bool("full_text_rich_view", false) && rich_view_ft.as_ref().is_some() {
                spawn_rich_fetch(&tx_ft, &article);
            }
        });
        {
            let a = act_fulltext.clone();
            fulltext_btn.on_click(move |_| a());
        }

        // Translate Article: send the reader's text (or the
        // article body) to the configured LLM provider on a
        // worker; the translation replaces the reader content.
        let state_tl = state.clone();
        let frame_tl = frame;
        let tx_tl = tx.clone();
        let act_translate: Rc<dyn Fn()> = Rc::new(move || {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if !cfg.get_bool("translation_enabled", false) {
                frame_tl.set_status_text("Translation is disabled — enable it in Settings", 0);
                return;
            }
            let (article, text) = {
                let st = state_tl.borrow();
                let Some(article) = st
                    .selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
                else {
                    frame_tl.set_status_text("No article selected", 0);
                    return;
                };
                let shown = reader.get_value();
                let text = if shown.trim().is_empty() {
                    article
                        .content
                        .clone()
                        .or(article.description.clone())
                        .unwrap_or_default()
                } else {
                    shown
                };
                (article, text)
            };
            if text.trim().is_empty() {
                frame_tl.set_status_text("Nothing to translate", 0);
                return;
            }
            // Normalize the configured target to a BCP-47 tag
            // (article_lang.normalize_lang); junk falls back to
            // English rather than reaching the provider as-is.
            let target = blindrss_core::article_lang::normalize_lang(Some(
                cfg.get_str("translation_target_language", "en"),
            ))
            .unwrap_or_else(|| "en".to_string());
            // Skip work when the text already reads as the
            // target language (article_lang priority: declared
            // language wins — here the heuristic over the text).
            let detected = blindrss_core::translate::detect_language(&text);
            let target_primary = target.split('-').next().unwrap_or("en");
            if !detected.is_empty() && detected == target_primary {
                frame_tl.set_status_text("Article is already in the target language", 0);
                return;
            }
            frame_tl.set_status_text("Translating...", 0);
            // The result is applied only under this exact
            // identity (Audit 8 O08): same article, same
            // reader generation, same provider/language/model
            // variant. Bumping the generation also retires
            // any in-flight full-text application, like
            // Python's mode/token change.
            let request = {
                let mut st = state_tl.borrow_mut();
                st.reader_generation = st.reader_generation.wrapping_add(1);
                st.pending_fulltext = None;
                st.auto_load = None;
                blindrss_core::reader_cache::TranslationRequest {
                    article_id: article.id.clone(),
                    generation: st.reader_generation,
                    variant: translation_variant(&cfg),
                }
            };
            let tx = tx_tl.clone();
            std::thread::spawn(move || {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let result = blindrss_core::translate::translate_text(&cfg, &text, &target);
                let _ = tx.send(RefreshMsg::Translated(request, result));
            });
        });

        // Toggle Read: flips the selected article locally and
        // propagates to the hosted provider when it came from one.
        let state_tr = state.clone();
        let frame_tr = frame;
        let list_tr = list;
        let act_toggle_read: Rc<dyn Fn()> = Rc::new(move || {
            let mut st = state_tr.borrow_mut();
            let indices = selected_article_indices(&list_tr, &st);
            if indices.is_empty() {
                frame_tr.set_status_text("No article selected", 0);
                return;
            }
            let articles: Vec<Article> = indices.iter().map(|&i| st.articles[i].clone()).collect();
            let new_state = !articles.iter().any(|a| a.is_read);
            for (i, article) in indices.iter().zip(&articles) {
                if let Some(db) = &st.db {
                    mark_article_read(db, article, new_state);
                }
                st.articles[*i].is_read = new_state;
            }
            refill_articles(&mut st, &list_tr);
            frame_tr.set_status_text(
                &format!(
                    "Marked {} article(s) {}",
                    articles.len(),
                    if new_state { "read" } else { "unread" }
                ),
                0,
            );
            announce("status_toggle", if new_state { "Read" } else { "Unread" });
            drop(st);
            for article in articles {
                propagate_read_state(&article, new_state);
            }
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
        let tx_ef = tx.clone();
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
            drop(st);
            // Hosted: the same edit goes to the server through
            // the provider (changed fields only).
            push_feed_update(
                &tx_ef,
                feed.clone(),
                if title.trim().is_empty() || title.trim() == feed.title {
                    None
                } else {
                    Some(title.trim().to_string())
                },
                if url.trim().is_empty() || url.trim() == feed.url {
                    None
                } else {
                    Some(url.trim().to_string())
                },
                None,
            );
            let mut st = state_ef.borrow_mut();
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
        let tx_rc = tx.clone();
        let act_rename_cat: Rc<dyn Fn()> = Rc::new(move || {
            let (category, db_ok) = {
                let st = state_rc.borrow();
                (selected_category(&st), st.db.is_some())
            };
            let Some(category) = category else {
                frame_rc.set_status_text("Selected feed has no category", 0);
                return;
            };
            if is_protected_uncategorized(Some(&category)) {
                frame_rc.set_status_text("The Uncategorized folder cannot be renamed.", 0);
                return;
            }
            if !db_ok {
                return;
            }
            // Python's rename prompt edits the LEAF only; the
            // node keeps its parent (Db::rename_category takes
            // the new leaf, like Python's provider method).
            let leaf = blindrss_core::category_display_leaf(&category);
            let Some(new_name) = prompt_focus(
                &frame_rc,
                "New category name:",
                "Rename Category",
                &leaf,
                &state_rc.borrow(),
                &tree_rc,
                &list_rc,
            ) else {
                return;
            };
            let new_leaf = blindrss_core::sanitize_category_leaf(&new_name);
            if new_leaf.is_empty() || new_leaf == leaf {
                return;
            }
            let new_path = blindrss_core::make_category_path(
                blindrss_core::category_parent_path(&category).as_deref(),
                &new_leaf,
            );
            let mapping = {
                let st = state_rc.borrow();
                st.db
                    .as_ref()
                    .and_then(|db| db.categories().ok())
                    .map(|cats| {
                        blindrss_core::category_subtree_mapping(
                            &cats.into_iter().map(|c| c.title).collect::<Vec<_>>(),
                            &category,
                            &new_path,
                        )
                    })
                    .unwrap_or_default()
            };
            let mut st = state_rc.borrow_mut();
            let renamed = match &st.db {
                Some(db) => db.rename_category(&category, &new_leaf),
                None => Ok(false),
            };
            if let Some(db) = &st.db {
                st.feeds = db.feeds().unwrap_or_default();
            }
            drop(st);
            if !matches!(renamed, Ok(true)) {
                frame_rc.set_status_text("Category not renamed (missing, or the name is taken)", 0);
                return;
            }
            remap_global_delete_behavior(&mapping);
            mirror_hosted_category(&tx_rc, Some((category.clone(), new_path)), None);
            let mut st = state_rc.borrow_mut();
            rebuild_tree(&mut st, &tree_rc);
            refill_articles(&mut st, &list_rc);
            frame_rc.set_status_text("Category renamed", 0);
        });
        {
            let a = act_rename_cat.clone();
            rename_cat_btn.on_click(move |_| a());
        }

        // Edit Category (tree context / feeds.edit_selected):
        // Python's CategoryPropertiesDialog (Batch 41a) — rename
        // the leaf and/or move under another category, applied
        // as Python does: move first (Db::move_category), then
        // rename the leaf (Db::rename_category).
        let state_ec = state.clone();
        let frame_ec = frame;
        let tree_ec = tree;
        let list_ec = list;
        let tx_ec = tx.clone();
        let act_edit_category: Rc<dyn Fn()> = Rc::new(move || {
            let (category, db_ok) = {
                let st = state_ec.borrow();
                (selected_category(&st), st.db.is_some())
            };
            let Some(category) = category else {
                frame_ec.set_status_text("Select a category in the tree first", 0);
                return;
            };
            if is_protected_uncategorized(Some(&category)) {
                frame_ec.set_status_text("The Uncategorized folder cannot be renamed or moved.", 0);
                return;
            }
            if !db_ok {
                return;
            }
            let new_path = {
                let st = state_ec.borrow();
                let Some(db) = st.db.as_ref() else { return };
                dialogs::show_category_properties(&frame_ec, db, &category)
            };
            restore_focus(&state_ec.borrow(), &tree_ec, &list_ec);
            let Some(new_path) = new_path else { return };
            if new_path == category {
                return;
            }
            let (new_leaf, new_parent) = (
                blindrss_core::category_display_leaf(&new_path),
                blindrss_core::category_parent_path(&new_path),
            );
            let old_leaf = blindrss_core::category_display_leaf(&category);
            let old_parent = blindrss_core::category_parent_path(&category);
            let titles: Vec<String> = {
                let st = state_ec.borrow();
                st.db
                    .as_ref()
                    .and_then(|db| db.categories().ok())
                    .map(|cats| cats.into_iter().map(|c| c.title).collect())
                    .unwrap_or_default()
            };
            let mapping = blindrss_core::category_subtree_mapping(&titles, &category, &new_path);
            let mut st = state_ec.borrow_mut();
            if let Some(db) = &st.db {
                if let Some(parent) = &new_parent {
                    let _ = db.ensure_category_path(parent);
                }
                // Move first, then rename (Python's order: the
                // only collision that can block is at the
                // destination).
                let mut current = category.clone();
                if new_parent != old_parent {
                    if matches!(db.move_category(&current, new_parent.as_deref()), Ok(true)) {
                        current =
                            blindrss_core::make_category_path(new_parent.as_deref(), &old_leaf);
                    } else {
                        drop(st);
                        frame_ec.set_status_text(
                            "Category could not be moved (name taken at the destination)",
                            0,
                        );
                        return;
                    }
                }
                if new_leaf != old_leaf {
                    let _ = db.rename_category(&current, &new_leaf);
                }
                st.feeds = db.feeds().unwrap_or_default();
            }
            drop(st);
            remap_global_delete_behavior(&mapping);
            mirror_hosted_category(&tx_ec, Some((category.clone(), new_path.clone())), None);
            let mut st = state_ec.borrow_mut();
            rebuild_tree(&mut st, &tree_ec);
            refill_articles(&mut st, &list_ec);
            frame_ec.set_status_text("Category updated", 0);
        });

        // Set Feed Category: prompt (blank = uncategorized); a new
        // name creates the category.
        let state_sc = state.clone();
        let frame_sc = frame;
        let tree_sc = tree;
        let list_sc = list;
        let tx_sc = tx.clone();
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
            drop(st);
            // Hosted: reassignment is a server change too —
            // a label add/remove for the GReader family, a
            // category_id change for Miniflux — through
            // provider.update_feed with the pre-edit feed as
            // the snapshot (its category is the old label).
            // Some("") clears the category.
            if name != feed.category.as_deref().unwrap_or("") {
                push_feed_update(&tx_sc, feed.clone(), None, None, Some(name.to_string()));
            }
            let mut st = state_sc.borrow_mut();
            rebuild_tree(&mut st, &tree_sc);
            refill_articles(&mut st, &list_sc);
            frame_sc.set_status_text("Feed category set", 0);
        });
        {
            let a = act_set_cat.clone();
            set_cat_btn.on_click(move |_| a());
        }

        // Resolve media and its request headers on a worker.
        let state_pm = state.clone();
        let frame_pm = frame;
        let tx_pm = tx.clone();
        let act_play: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_pm.borrow();
            let Some(mut article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_pm.set_status_text("No article selected", 0);
                return;
            };
            // Media detection scans the body for embedded
            // media; list rows are light, so resolve it first.
            ensure_content(st.db.as_ref(), &mut article);
            drop(st);
            state_pm.borrow_mut().pending_play = Some(article.clone());
            frame_pm.set_status_text("Finding playable media...", 0);
            let tx = tx_pm.clone();
            let id = article.id.clone();
            let path = db_path();
            std::thread::spawn(move || {
                let result = blindrss_core::media::resolve_article_for_play(
                    &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
                    &article,
                )
                .map(|r| {
                    if r.detected
                        && let Ok(db) = Db::open(&path)
                    {
                        let _ =
                            db.update_article_media(&article.id, &r.url, r.media_type.as_deref());
                    }
                    let info = if r.via_ytdlp {
                        blindrss_core::media::info(&article.url).ok()
                    } else {
                        None
                    };
                    let url = maybe_cache_youtube(&article, &r.url, r.via_ytdlp);
                    let headers = playback_headers_for(&article, &r.url, &path);
                    (url, info, headers)
                });
                let _ = tx.send(RefreshMsg::MediaReady(id, result));
            });
        });
        {
            let a = act_play.clone();
            play_btn.on_click(move |_| a());
        }
        *tray_play_slot.borrow_mut() = Some(act_play.clone());

        // Article activation (Enter or double-click in the
        // article list — Python's on_article_activate): an
        // article with playable media plays in the player;
        // anything else opens in the browser. The article is
        // marked read either way, as Python does.
        let state_act = state.clone();
        let frame_act = frame;
        let list_act = list;
        let act_play_for_act = act_play.clone();
        let act_activate: Rc<dyn Fn()> = Rc::new(move || {
            let mut st = state_act.borrow_mut();
            // The trailing placeholder row of a paged view:
            // Enter loads the next page (Python's "Load more
            // items (Enter)").
            if st.selected_article == Some(st.articles.len())
                && st.view_total > st.articles.len() as i64
            {
                load_more_articles(&mut st, &list_act);
                return;
            }
            let Some(mut article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_act.set_status_text("No article selected", 0);
                return;
            };
            // The activation decision's last step detects media
            // in the body; list rows are light, so resolve it.
            ensure_content(st.db.as_ref(), &mut article);
            // Activation records the article opened (Python's
            // _open_article -> _mark_article_opened; Audit 8
            // G01) — read marking follows below.
            if let Some(db) = &st.db {
                let _ = db.mark_opened(&article.id);
            }
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let playable = blindrss_core::download::downloaded_media_path(&cfg, &article).is_some()
                || article.media_url.as_deref().is_some_and(|u| {
                    !u.is_empty()
                        && blindrss_core::media::is_playable_media(u, article.media_type.as_deref())
                })
                || blindrss_core::media::is_ytdlp_supported(&article.url)
                || article.content.as_deref().is_some_and(|html| {
                    blindrss_core::media::detect_media_in_page(html, &article.url).is_some()
                });
            let was_unread = !article.is_read;
            if was_unread {
                if let Some(db) = &st.db {
                    mark_article_read(db, &article, true);
                }
                if let Some(row) = st.articles.iter_mut().find(|a| a.id == article.id) {
                    row.is_read = true;
                }
            }
            drop(st);
            if was_unread {
                propagate_read_state(&article, true);
            }
            if playable {
                act_play_for_act();
            } else if !article.url.is_empty() {
                open_article_link(&frame_act, &article.url);
            } else {
                frame_act.set_status_text("Article has no media or link", 0);
            }
        });
        {
            let a = act_activate.clone();
            list.on_item_activated(move |_| a());
        }

        // New Smart Folder: the full condition editor dialog
        // (Python's SmartFolderDialog).
        let state_nsf = state.clone();
        let frame_nsf = frame;
        let tree_nsf = tree;
        let list_nsf = list;
        let act_new_sf: Rc<dyn Fn()> = Rc::new(move || {
            let empty = serde_json::json!({"match": "all", "conditions": []});
            let Some((name, rule)) =
                dialogs::edit_smart_folder(&frame_nsf, "New Smart Folder", "", &empty)
            else {
                restore_focus(&state_nsf.borrow(), &tree_nsf, &list_nsf);
                return;
            };
            let mut st = state_nsf.borrow_mut();
            if let Some(db) = &st.db {
                let _ = db.create_smart_folder(&name, &rule.to_string());
            }
            rebuild_tree(&mut st, &tree_nsf);
            refill_articles(&mut st, &list_nsf);
            frame_nsf.set_status_text("Smart folder created", 0);
            restore_focus(&st, &tree_nsf, &list_nsf);
        });
        {
            let a = act_new_sf.clone();
            new_sf_btn.on_click(move |_| a());
        }

        // Edit Smart Folder: the same editor over the selected
        // smart folder's stored rule.
        let state_esf = state.clone();
        let frame_esf = frame;
        let tree_esf = tree;
        let list_esf = list;
        let act_edit_sf: Rc<dyn Fn()> = Rc::new(move || {
            let folder = {
                let st = state_esf.borrow();
                let Some(id) = st
                    .selected_feed
                    .as_ref()
                    .and_then(|f| f.strip_prefix("smart:"))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                else {
                    frame_esf.set_status_text("Select a smart folder in the tree first", 0);
                    return;
                };
                st.db.as_ref().and_then(|db| {
                    db.smart_folders().ok().and_then(|folders| {
                        folders
                            .into_iter()
                            .find(|(fid, ..)| *fid == id)
                            .map(|(_, name, rule_json, _)| (id, name, rule_json))
                    })
                })
            };
            let Some((id, name0, rule_json)) = folder else {
                return;
            };
            let rule0: serde_json::Value =
                serde_json::from_str(&rule_json).unwrap_or(serde_json::Value::Null);
            let Some((name, rule)) =
                dialogs::edit_smart_folder(&frame_esf, "Edit Smart Folder", &name0, &rule0)
            else {
                restore_focus(&state_esf.borrow(), &tree_esf, &list_esf);
                return;
            };
            let mut st = state_esf.borrow_mut();
            if let Some(db) = &st.db {
                let _ = db.update_smart_folder(&id, &name, &rule.to_string());
            }
            rebuild_tree(&mut st, &tree_esf);
            refill_articles(&mut st, &list_esf);
            frame_esf.set_status_text("Smart folder updated", 0);
            restore_focus(&st, &tree_esf, &list_esf);
        });

        // Filter Rules manager (Tools): list/editor/reorder/
        // apply-to-existing dialog; it works on its own database
        // connections, modal, then we refill (Apply may have
        // changed article states).
        let state_fr = state.clone();
        let frame_fr = frame;
        let tree_fr = tree;
        let list_fr = list;
        let act_filter_rules: Rc<dyn Fn()> = Rc::new(move || {
            {
                let st = state_fr.borrow();
                let Some(db) = st.db.as_ref() else { return };
                dialogs::show_filter_rules(&frame_fr, db);
            }
            let mut st = state_fr.borrow_mut();
            refill_articles(&mut st, &list_fr);
            restore_focus(&st, &tree_fr, &list_fr);
        });

        // Feed Properties (Feeds menu / tree context menu).
        let state_fp = state.clone();
        let frame_fp = frame;
        let tree_fp = tree;
        let list_fp = list;
        let tx_fp = tx.clone();
        let act_feed_props: Rc<dyn Fn()> = Rc::new(move || {
            let feed = {
                let st = state_fp.borrow();
                st.selected_feed
                    .as_ref()
                    .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned())
            };
            let Some(feed) = feed else {
                frame_fp.set_status_text("Select a feed in the tree first", 0);
                return;
            };
            // `feed` is the pre-dialog snapshot (title/url/
            // category). The dialog applies its edits to the
            // cache Db directly; for a hosted feed the diff
            // against this snapshot is then pushed through
            // provider.update_feed on a worker (remote edit
            // first, idempotent cache re-apply after success;
            // a remote failure surfaces in the status bar).
            // The encoding override stays local-only — hosted
            // providers have no such server field.
            let changed = {
                let st = state_fp.borrow();
                match st.db.as_ref() {
                    Some(db) => dialogs::show_feed_properties(&frame_fp, db, &feed),
                    None => false,
                }
            };
            if changed {
                let after = {
                    let mut st = state_fp.borrow_mut();
                    st.feeds = st
                        .db
                        .as_ref()
                        .and_then(|db| db.feeds().ok())
                        .unwrap_or_default();
                    rebuild_tree(&mut st, &tree_fp);
                    refill_articles(&mut st, &list_fp);
                    st.feeds.iter().find(|f| f.id == feed.id).cloned()
                };
                if let Some(after) = after {
                    push_feed_update(
                        &tx_fp,
                        feed.clone(),
                        if after.title != feed.title {
                            Some(after.title.clone())
                        } else {
                            None
                        },
                        if after.url != feed.url {
                            Some(after.url.clone())
                        } else {
                            None
                        },
                        if after.category != feed.category {
                            Some(after.category.clone().unwrap_or_default())
                        } else {
                            None
                        },
                    );
                }
            }
            restore_focus(&state_fp.borrow(), &tree_fp, &list_fp);
        });

        // About BlindRSS (Help menu).
        let frame_about = frame;
        let state_about = state.clone();
        let tree_about = tree;
        let list_about = list;
        let act_about: Rc<dyn Fn()> = Rc::new(move || {
            dialogs::show_about(&frame_about);
            restore_focus(&state_about.borrow(), &tree_about, &list_about);
        });

        // Open Article from URL (Tools): dialog, then the
        // article window loads the page itself (Audit 8 G13).
        let frame_oa = frame;
        let act_open_article: Rc<dyn Fn()> = Rc::new(move || {
            // Python on_open_article_url (Audit 8 G13): the
            // dialog is prefilled with the clipboard contents
            // when they are a usable URL, offers the starting
            // reader, and the article window opens immediately
            // — the page loads inside it, asynchronously,
            // instead of this command blocking on a plain-text
            // prefetch before any window appears.
            let clipboard_url = wxdragon::prelude::Clipboard::get()
                .get_text()
                .map(|t| blindrss_core::media_url::normalize_media_url(t.trim()))
                .unwrap_or_default();
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let Some((typed, rich)) = dialogs::show_open_article_dialog(
                &frame_oa,
                &clipboard_url,
                cfg.get_bool("full_text_rich_view", false),
            ) else {
                return;
            };
            let url = blindrss_core::media_url::normalize_media_url(typed.trim());
            if url.is_empty() {
                let message = if typed.trim().is_empty() {
                    "Paste the address of an article first."
                } else {
                    "That does not look like an article address. Paste the full address, starting with http:// or https://."
                };
                let dialog =
                    wxdragon::prelude::MessageDialog::builder(&frame_oa, message, "Open Article")
                        .with_style(
                            wxdragon::prelude::MessageDialogStyle::OK
                                | wxdragon::prelude::MessageDialogStyle::IconInformation,
                        )
                        .build();
                dialog.show_modal();
                dialog.destroy();
                return;
            }
            article_window::open_url(&url, rich);
        });

        // Open Media URL (Tools): play an arbitrary media URL in
        // the player.
        let frame_om = frame;
        let state_om = state.clone();
        let tree_om = tree;
        let list_om = list;
        let tx_om = tx.clone();
        let act_open_media: Rc<dyn Fn()> = Rc::new(move || {
            let Some(url) = prompt_focus(
                &frame_om,
                "Media URL:",
                "Open Media URL",
                "",
                &state_om.borrow(),
                &tree_om,
                &list_om,
            ) else {
                return;
            };
            // Clean the paste the Python way (media_url.py):
            // scheme-less and protocol-relative pastes work,
            // search phrases and mailto:/magnet: do not.
            let url = blindrss_core::media_url::normalize_media_url(url.trim());
            if url.is_empty() {
                frame_om.set_status_text("That does not look like a media URL", 0);
                return;
            }
            let article = blindrss_core::Article {
                id: url.clone(),
                feed_id: String::new(),
                title: blindrss_core::media_url::title_from_url(&url),
                url: url.clone(),
                content: None,
                description: None,
                date: None,
                author: None,
                is_read: true,
                is_favorite: false,
                media_url: Some(url.clone()),
                media_type: None,
            };
            state_om.borrow_mut().pending_play = Some(article.clone());
            frame_om.set_status_text("Loading media...", 0);
            let tx = tx_om.clone();
            let path = db_path();
            std::thread::spawn(move || {
                let headers = playback_headers_for(&article, &url, &path);
                let _ = tx.send(RefreshMsg::MediaReady(article.id, Ok((url, None, headers))));
            });
        });

        // Video Search (Tools): cross-site yt-dlp search dialog;
        // Play resolves through the normal playback path,
        // Subscribe runs the result URL through feed discovery.
        let frame_vs = frame;
        let state_vs = state.clone();
        let tx_vs = tx.clone();
        let act_video_search: Rc<dyn Fn()> =
            Rc::new(
                move || match dialogs::show_video_search(&frame_vs, &config_path()) {
                    Some(("play", result)) => {
                        let article = blindrss_core::Article {
                            id: result.url.clone(),
                            feed_id: String::new(),
                            title: result.title.clone(),
                            url: result.url.clone(),
                            content: None,
                            description: None,
                            date: None,
                            author: result.uploader.clone(),
                            is_read: true,
                            is_favorite: false,
                            media_url: Some(result.url.clone()),
                            media_type: None,
                        };
                        {
                            let mut st = state_vs.borrow_mut();
                            st.pending_play = Some(article.clone());
                        }
                        frame_vs.set_status_text("Loading...", 0);
                        let tx = tx_vs.clone();
                        let path = db_path();
                        std::thread::spawn(move || {
                            let id = article.id.clone();
                            let resolved = blindrss_core::media::resolve_article_for_play(
                                &blindrss_core::config::Config::load(&config_path())
                                    .unwrap_or_default(),
                                &article,
                            )
                            .map(|r| {
                                let info = if r.via_ytdlp {
                                    blindrss_core::media::info(&article.url).ok()
                                } else {
                                    None
                                };
                                let headers = playback_headers_for(&article, &r.url, &path);
                                (r.url, info, headers)
                            });
                            let _ = tx.send(RefreshMsg::MediaReady(id, resolved));
                        });
                    }
                    Some(("subscribe", result)) => {
                        let url = result.url.clone();
                        frame_vs.set_status_text(&format!("Adding {url}..."), 0);
                        let tx = tx_vs.clone();
                        std::thread::spawn(move || {
                            let result = subscribe_on_worker(&url, None);
                            let _ = tx.send(RefreshMsg::Added(url, result, None));
                        });
                    }
                    _ => {}
                },
            );

        // Persistent Search (Tools): manage saved searches and
        // run one into the article list via the existing search.
        let frame_ps = frame;
        let act_search_ps = act_search.clone();
        let act_persistent: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(query) = dialogs::show_persistent_searches(&frame_ps, &config_path()) {
                search_box.set_value(&query);
                act_search_ps();
            }
        });

        // Add Shortcuts (Batch 42): Python's
        // AddShortcutsDialog.
        let frame_as = frame;
        let act_add_shortcuts: Rc<dyn Fn()> = Rc::new(move || {
            dialogs::show_add_shortcuts(&frame_as);
        });

        // Article Window (Article menu): the selected article in
        // its own window. With the rich view on, the window gets
        // the HTML rendering too and offers Python's "Show in HTML
        // view" switch; the render runs on a worker.
        let state_aw = state.clone();
        let frame_aw = frame;
        let act_article_window: Rc<dyn Fn()> = Rc::new(move || {
            let st = state_aw.borrow();
            let Some(article) = st
                .selected_article
                .and_then(|i| st.articles.get(i))
                .cloned()
            else {
                frame_aw.set_status_text("No article selected", 0);
                return;
            };
            let body = reader.get_value();
            let body = if body.trim().is_empty() {
                article
                    .content
                    .clone()
                    .or(article.description.clone())
                    .unwrap_or_default()
            } else {
                body
            };
            article_window::open(&article.title, &body, Some(&article.url));
        });

        // Podcast Archive (Tools): pick a podcast feed, rescan
        // it against the Wayback Machine (same worker flow as
        // the tree's Rescan Archive).
        let state_pa = state.clone();
        let frame_pa = frame;
        let tree_pa = tree;
        let list_pa = list;
        let tx_pa = tx.clone();
        let act_podcast_archive: Rc<dyn Fn()> = Rc::new(move || {
            let feed = {
                let st = state_pa.borrow();
                let Some(db) = st.db.as_ref() else { return };
                dialogs::show_podcast_archive(&frame_pa, db)
            };
            restore_focus(&state_pa.borrow(), &tree_pa, &list_pa);
            let Some(feed) = feed else { return };
            frame_pa.set_status_text("Rescanning archive...", 0);
            let tx = tx_pa.clone();
            std::thread::spawn(move || {
                let result = Db::open(&db_path())
                    .map_err(|e| format!("{e}"))
                    .and_then(|db| {
                        let global_behavior = blindrss_core::config::Config::load(&config_path())
                            .map(|c| c.get_str("delete_behavior", "deleted").to_string())
                            .unwrap_or_else(|_| "deleted".to_string());
                        blindrss_core::archive::rescan_archive(&db, &feed, &global_behavior)
                    });
                let _ = tx.send(RefreshMsg::ArchiveDone(feed.title.clone(), result));
            });
        });

        // Play queue (config "play_queue", Python's model):
        // start playback at an entry, resolving on a worker
        // exactly like Play; the player advances on finish.
        let state_qp = state.clone();
        let frame_qp = frame;
        let tx_qp = tx.clone();
        let act_play_queue_index: Rc<dyn Fn(usize)> = Rc::new(move |index: usize| {
            let entries = dialogs::load_queue(&config_path());
            let Some(_entry) = entries.get(index) else {
                return;
            };
            let stubs: Vec<Article> = entries.iter().map(queue_entry_article).collect();
            let article = stubs[index].clone();
            {
                let mut st = state_qp.borrow_mut();
                st.pending_play = Some(article.clone());
                st.pending_queue = Some((stubs, index));
            }
            frame_qp.set_status_text("Loading queue entry...", 0);
            let tx = tx_qp.clone();
            let path = db_path();
            std::thread::spawn(move || {
                let id = article.id.clone();
                let result = blindrss_core::media::resolve_article_for_play(
                    &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
                    &article,
                )
                .map(|r| {
                    let info = if r.via_ytdlp {
                        blindrss_core::media::info(&article.url).ok()
                    } else {
                        None
                    };
                    let headers = playback_headers_for(&article, &r.url, &path);
                    (r.url, info, headers)
                });
                let _ = tx.send(RefreshMsg::MediaReady(id, result));
            });
        });

        // Toggle Queue: add or remove the selected article.
        let state_tq = state.clone();
        let frame_tq = frame;
        let act_toggle_queue: Rc<dyn Fn()> = Rc::new(move || {
            let article = {
                let st = state_tq.borrow();
                st.selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
            };
            let Some(article) = article else {
                frame_tq.set_status_text("No article selected", 0);
                return;
            };
            let mut entries = dialogs::load_queue(&config_path());
            if let Some(pos) = entries.iter().position(|e| {
                e.get("article_id").and_then(|v| v.as_str()) == Some(article.id.as_str())
            }) {
                entries.remove(pos);
                dialogs::save_queue(&config_path(), &entries);
                frame_tq.set_status_text("Removed from play queue", 0);
                return;
            }
            let media_url = article
                .media_url
                .clone()
                .unwrap_or_else(|| article.url.clone());
            let feed_title = {
                let st = state_tq.borrow();
                st.feeds
                    .iter()
                    .find(|f| f.id == article.feed_id)
                    .map(|f| f.title.clone())
                    .unwrap_or_default()
            };
            entries.push(serde_json::json!({
                "article_id": article.id,
                "media_url": media_url,
                "use_ytdlp": article.media_url.is_none(),
                "title": article.title,
                "media_type": article.media_type.clone().unwrap_or_default(),
                "feed_id": article.feed_id,
                "feed_title": feed_title,
            }));
            dialogs::save_queue(&config_path(), &entries);
            frame_tq.set_status_text("Added to play queue", 0);
        });

        // Play Queue dialog (Article menu / queue.open).
        let frame_qo = frame;
        let state_qo = state.clone();
        let tree_qo = tree;
        let list_qo = list;
        let play_q = act_play_queue_index.clone();
        let act_queue_open: Rc<dyn Fn()> = Rc::new(move || {
            let picked = dialogs::show_queue(&frame_qo, &config_path());
            restore_focus(&state_qo.borrow(), &tree_qo, &list_qo);
            if let Some(index) = picked {
                play_q(index);
            }
        });

        // Queue Next / Previous: start from the mirrored
        // play_queue_index (the player keeps it current).
        let play_q2 = act_play_queue_index.clone();
        let frame_qn = frame;
        let act_queue_next: Rc<dyn Fn()> = Rc::new(move || {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let cur = cfg.get_i64("play_queue_index", -1);
            let len = dialogs::load_queue(&config_path()).len() as i64;
            let next = cur + 1;
            if next >= 0 && next < len {
                play_q2(next as usize);
            } else {
                frame_qn.set_status_text("No next queue entry", 0);
            }
        });
        let play_q3 = act_play_queue_index.clone();
        let frame_qv = frame;
        let act_queue_prev: Rc<dyn Fn()> = Rc::new(move || {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let cur = cfg.get_i64("play_queue_index", -1);
            let prev = cur - 1;
            if prev >= 0 {
                play_q3(prev as usize);
            } else {
                frame_qv.set_status_text("No previous queue entry", 0);
            }
        });

        // Feed Search (Tools): discovery dialog; subscribing
        // runs the same resolve-and-add flow as Add Feed.
        let state_fs = state.clone();
        let frame_fs = frame;
        let tree_fs = tree;
        let list_fs = list;
        let tx_fs = tx.clone();
        let act_feed_search: Rc<dyn Fn()> = Rc::new(move || {
            let Some(url) = dialogs::show_feed_search(&frame_fs) else {
                restore_focus(&state_fs.borrow(), &tree_fs, &list_fs);
                return;
            };
            restore_focus(&state_fs.borrow(), &tree_fs, &list_fs);
            {
                let st = state_fs.borrow();
                if st.feeds.iter().any(|f| f.url == url) {
                    frame_fs.set_status_text("That feed is already subscribed", 0);
                    return;
                }
            }
            frame_fs.set_status_text(&format!("Adding {url}..."), 0);
            let tx = tx_fs.clone();
            std::thread::spawn(move || {
                let result = subscribe_on_worker(&url, None);
                let _ = tx.send(RefreshMsg::Added(url, result, None));
            });
        });

        // Import Site Cookies (Tools): Chromium profile cookies
        // merged into the jar beside the config.
        let state_ic = state.clone();
        let frame_ic = frame;
        let tree_ic = tree;
        let list_ic = list;
        let act_import_cookies: Rc<dyn Fn()> = Rc::new(move || {
            dialogs::show_import_cookies(&frame_ic, &config_path());
            restore_focus(&state_ic.borrow(), &tree_ic, &list_ic);
        });

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
        let download_action = |choose_format: bool| -> Rc<dyn Fn()> {
            let state = state.clone();
            let tx = tx.clone();
            Rc::new(move || {
                let articles: Vec<Article> = {
                    let st = state.borrow();
                    selected_article_indices(&list, &st)
                        .into_iter()
                        .map(|i| {
                            // Download resolution scans bodies
                            // for media; list rows are light.
                            let mut a = st.articles[i].clone();
                            ensure_content(st.db.as_ref(), &mut a);
                            a
                        })
                        .collect()
                };
                if articles.is_empty() {
                    frame.set_status_text("No article selected", 0);
                    return;
                }
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if !cfg.get_bool("downloads_enabled", false) {
                    frame.set_status_text("Downloads are disabled — enable them in Settings", 0);
                    return;
                }
                let format = if choose_format {
                    let labels: Vec<&str> = blindrss_core::download::DOWNLOAD_FORMAT_IDS
                        .iter()
                        .map(|id| blindrss_core::download::download_format_label(id))
                        .collect();
                    let dialog = SingleChoiceDialog::builder(
                        &frame,
                        "Download format:",
                        "Download As",
                        &labels,
                    )
                    .build();
                    let result = dialog.show_modal();
                    let selected = dialog.get_selection();
                    dialog.destroy();
                    restore_focus(&state.borrow(), &tree, &list);
                    if result != ID_OK {
                        return;
                    }
                    blindrss_core::download::DOWNLOAD_FORMAT_IDS
                        .get(selected as usize)
                        .copied()
                        .unwrap_or(blindrss_core::download::DOWNLOAD_FORMAT_DEFAULT)
                        .to_string()
                } else {
                    cfg.get_str("download_format", "video_best").to_string()
                };
                let dir = PathBuf::from(cfg.get_str("download_path", ""));
                let dir = if dir.as_os_str().is_empty() {
                    home_dir().join("Downloads").join("BlindRSS")
                } else {
                    dir
                };
                frame.set_status_text("Downloading...", 0);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for article in articles {
                        let result = if let Some(media) = article.media_url.as_deref().filter(|u| {
                            !u.is_empty()
                                && blindrss_core::media::is_playable_media(
                                    u,
                                    article.media_type.as_deref(),
                                )
                                && !blindrss_core::media::is_ytdlp_supported(u)
                        }) {
                            blindrss_core::download::download_direct(media, &dir, &article.title)
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
                                Ok(r) if r.via_ytdlp => {
                                    blindrss_core::download::download_with_ytdlp(
                                        &article.url,
                                        &dir,
                                        &format,
                                    )
                                }
                                Ok(r) => blindrss_core::download::download_direct(
                                    &r.url,
                                    &dir,
                                    &article.title,
                                ),
                                Err(e) => Err(e),
                            }
                        } else {
                            Err("Article has nothing to download".to_string())
                        };
                        let _ = tx.send(RefreshMsg::DownloadDone(article, result));
                    }
                });
            })
        };
        let act_download = download_action(false);
        let act_download_as = download_action(true);
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
                    let Some(feeds) = dialogs::select_takeout_feeds(&frame_to, &feeds) else {
                        restore_focus(&state_to.borrow(), &tree_to, &list_to);
                        return;
                    };
                    restore_focus(&state_to.borrow(), &tree_to, &list_to);
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
                    .and_then(|db| {
                        let global_behavior = blindrss_core::config::Config::load(&config_path())
                            .map(|c| c.get_str("delete_behavior", "deleted").to_string())
                            .unwrap_or_else(|_| "deleted".to_string());
                        blindrss_core::archive::rescan_archive(&db, &feed, &global_behavior)
                    });
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
            let path = path.clone();
            std::thread::spawn(move || {
                let result =
                    (|| -> Result<(Vec<blindrss_core::cast::CastDevice>, String), String> {
                        let resolved = blindrss_core::media::resolve_article_for_play(
                            &blindrss_core::config::Config::load(&config_path())
                                .unwrap_or_default(),
                            &article,
                        )?;
                        let mut devices =
                            blindrss_core::cast::discover_all(std::time::Duration::from_secs(3));
                        // Configured Kodi hosts join discovery
                        // (mDNS does not reach every network).
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        if let Some(hosts) =
                            cfg.raw().get("cast_kodi_hosts").and_then(|v| v.as_array())
                        {
                            for h in hosts.iter().filter_map(|v| v.as_str()) {
                                let (host, port) = match h.rsplit_once(':') {
                                    Some((host, p)) => {
                                        (host.to_string(), p.parse().unwrap_or(8080))
                                    }
                                    None => (h.to_string(), 8080),
                                };
                                if !devices.iter().any(|d| d.host == host) {
                                    devices.push(blindrss_core::cast::CastDevice {
                                        name: format!("Kodi {host}"),
                                        host,
                                        port,
                                        kind: "kodi".to_string(),
                                        model: String::new(),
                                        endpoint: String::new(),
                                    });
                                }
                            }
                        }
                        if devices.is_empty() {
                            return Err("No cast devices found on this network".to_string());
                        }
                        let headers = playback_headers_for(&article, &resolved.url, &path);
                        let url = blindrss_core::http_headers::protected_media_url(
                            &resolved.url,
                            &headers,
                        );
                        Ok((devices, url))
                    })();
                match result {
                    Ok((devices, url)) => {
                        let _ = tx.send(RefreshMsg::CastDevices(devices, article, url));
                    }
                    Err(e) => {
                        let _ = tx.send(RefreshMsg::CastDone(Err(e)));
                    }
                }
            });
        });
        {
            let a = act_cast.clone();
            cast_btn.on_click(move |_| a());
        }

        // Stop AirPlay Cast: ends the RAOP stream (the other
        // protocols play autonomously on the device).
        let frame_sac = frame;
        let act_stop_cast: Rc<dyn Fn()> = Rc::new(move || {
            match raop_stop_registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                Some(flag) => {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    frame_sac.set_status_text("AirPlay cast stopping", 0);
                }
                None => frame_sac.set_status_text("No AirPlay cast is running", 0),
            }
        });

        // Backup / Restore / Prune / Updates.
        let frame_bk = frame;
        let state_bk = state.clone();
        let tree_bk = tree;
        let list_bk = list;
        let tx_bk = tx.clone();
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
                // Python runs the backup on a worker (VACUUM + ZIP
                // of a large library must not block the UI) and
                // reports through the drain, where the completion
                // message carries the sensitive-data notice.
                frame_bk.set_status_text("Backing up data...", 0);
                let tx_bk = tx_bk.clone();
                std::thread::spawn(move || {
                    let dest = PathBuf::from(&path);
                    let result = blindrss_core::maintenance::create_backup(
                        &db_path(),
                        &config_path(),
                        &data_dir(),
                        &dest,
                    )
                    .map(|()| dest);
                    let _ = tx_bk.send(RefreshMsg::BackupDone(result));
                });
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
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let Some(days) = blindrss_core::retention::retention_days(
                cfg.get_str("article_retention", "unlimited"),
            ) else {
                frame_pr.set_status_text("Article retention is unlimited — nothing to prune", 0);
                return;
            };
            let mut st = state_pr.borrow_mut();
            match st.db.as_ref().map(|db| db.prune_old_articles(days)) {
                Some(Ok(n)) => {
                    refill_articles(&mut st, &list_pr);
                    frame_pr.set_status_text(&format!("Pruned {n} old article(s)"), 0);
                }
                Some(Err(e)) => frame_pr.set_status_text(&format!("Prune: {e}"), 0),
                None => frame_pr.set_status_text("No database", 0),
            }
        });
        // Check for Updates (Tools menu + Settings): ship-shape
        // runs the whole flow (check, dialog, verified download,
        // install/restart) on its own worker; Manual also reports
        // up-to-date and errors. ship-shape refuses overlapping
        // checks itself.
        let frame_up = frame;
        let act_update: Rc<dyn Fn()> = Rc::new(move || {
            update::check(&frame_up, ship_shape::ui::CheckTrigger::Manual);
        });
        let frame_up_auto = frame;
        let act_update_auto: Rc<dyn Fn()> = Rc::new(move || {
            update::check(&frame_up_auto, ship_shape::ui::CheckTrigger::Automatic);
        });

        // Settings: provider accounts + download folder + Inoreader
        // OAuth connect, in a modal dialog. Maintenance actions live
        // in the dialog now (Brandon, 2026-10-30: the main page was
        // crowded with them); the dialog reports which one was
        // chosen and it runs here, after the dialog closes.
        // Article Columns editor (from Settings): modal layout
        // dialog, then re-apply the layout to the article list.
        let frame_col = frame;
        let state_col = state.clone();
        let tree_col = tree;
        let list_col = list;
        let act_columns: Rc<dyn Fn()> = Rc::new(move || {
            dialogs::show_article_columns(&frame_col, &config_path());
            let mut st = state_col.borrow_mut();
            setup_article_columns(&list_col);
            refill_articles(&mut st, &list_col);
            restore_focus(&st, &tree_col, &list_col);
        });

        let frame_set = frame;
        let state_set = state.clone();
        let tree_set = tree;
        let list_set = list;
        let frame_eqd = frame;
        let act_equalizer: Rc<dyn Fn()> = Rc::new(move || {
            dialogs::show_equalizer(&frame_eqd, &config_path());
        });
        type Action = Rc<dyn Fn()>;
        let dispatch: Vec<(&'static str, Action)> = vec![
            ("backup", act_backup.clone()),
            ("restore", act_restore.clone()),
            ("takeout", act_takeout.clone()),
            ("archive", act_archive.clone()),
            ("sync", act_sync.clone()),
            ("prune", act_prune.clone()),
            ("update", act_update.clone()),
            ("columns", act_columns.clone()),
            ("equalizer", act_equalizer.clone()),
        ];
        let rich_view_set = rich_view.clone();
        let rich_loading_set = rich_loading.clone();
        let tx_set = tx.clone();
        let act_settings: Rc<dyn Fn()> = Rc::new(move || {
            #[cfg(target_os = "windows")]
            let prior_accel = blindrss_core::config::Config::load(&config_path())
                .map(|c| {
                    c.get_str("global_show_hide_hotkey", "Ctrl+Alt+B")
                        .to_string()
                })
                .unwrap_or_else(|_| "Ctrl+Alt+B".to_string());
            let prior_alt = blindrss_core::config::Config::load(&config_path())
                .map(|c| c.get_bool("show_image_alt", false))
                .unwrap_or(false);
            let intent = settings::show_settings(&frame_set, &config_path());
            apply_http_config();
            // Audit 8 G16: a global image-alt change
            // re-renders the current article immediately,
            // like the per-feed commands do.
            let now_alt = blindrss_core::config::Config::load(&config_path())
                .map(|c| c.get_bool("show_image_alt", false))
                .unwrap_or(false);
            if now_alt != prior_alt {
                rerender_current_article(
                    &state_set,
                    &frame_set,
                    &reader,
                    &rich_view_set,
                    &rich_loading_set,
                    &tx_set,
                );
            }
            let path = db_path();
            if state_set.borrow().store_path != path {
                let mut st = state_set.borrow_mut();
                st.cancel_refresh
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                st.cancel_refresh = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                st.refresh_stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                st.refreshing = false;
                st.last_refresh_end = None;
                st.store_path = path.clone();
                st.db = None;
                st.feeds.clear();
                st.articles.clear();
                st.view_total = -1;
                st.selected_feed = None;
                st.selected_article = None;
                st.pending_play = None;
                st.pending_queue = None;
                st.pending_fulltext = None;
                st.auto_load = None;
                st.reader_generation = st.reader_generation.wrapping_add(1);
                st.reader_cache = blindrss_core::reader_cache::ReaderCache::default();
                st.refresh_new.clear();
                st.refresh_failed = false;
                st.expanded_cats.clear();
                st.collapsed_cats.clear();
                match Db::open_or_create(&path) {
                    Ok(db) => {
                        st.feeds = db.feeds().unwrap_or_default();
                        st.db = Some(db);
                        frame_set.set_status_text("Account changed; refresh to sync its feeds", 0);
                    }
                    Err(e) => {
                        frame_set.set_status_text(&format!("Could not open account cache: {e}"), 0)
                    }
                }
                reader.clear();
                rebuild_tree(&mut st, &tree_set);
                refill_articles(&mut st, &list_set);
            }
            // Global hotkey change (Windows): register the new
            // combo; a rejected one reverts in config too, or the
            // bad value would be all that is left next launch
            // (Python's from_settings semantics).
            #[cfg(target_os = "windows")]
            {
                let mut cfg =
                    blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let new_accel = cfg
                    .get_str("global_show_hide_hotkey", "Ctrl+Alt+B")
                    .to_string();
                if new_accel != prior_accel {
                    let hwnd = frame_set.get_handle() as isize;
                    match hotkey::apply(hwnd, &new_accel) {
                        Ok(_) => {}
                        Err(e) => {
                            let prior = prior_accel.clone();
                            cfg.set_path("global_show_hide_hotkey", serde_json::json!(prior));
                            let _ = cfg.save(&config_path());
                            if matches!(e, hotkey::ApplyError::Taken) {
                                let msg = if prior.is_empty() {
                                    format!(
                                        "Another program is already using {new_accel}, so it cannot show or hide BlindRSS. The hotkey stays off."
                                    )
                                } else {
                                    format!(
                                        "Another program is already using {new_accel}, so it cannot show or hide BlindRSS. The hotkey stays {prior}."
                                    )
                                };
                                let md = MessageDialog::builder(&frame_set, &msg, "BlindRSS")
                                    .with_style(
                                        MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
                                    )
                                    .build();
                                md.show_modal();
                            } else {
                                frame_set.set_status_text(
                                    "That hotkey combination is not usable; keeping the current one",
                                    0,
                                );
                            }
                        }
                    }
                }
            }
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
            {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if blindrss_core::provider::active_name(&cfg) != "local" {
                    // Hosted backend: subscribe on the server,
                    // then pull it into the cache.
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                        let _ = tx.send(RefreshMsg::ProviderAction(provider.add_feed(&url)));
                    });
                    return;
                }
            }
            // Local: pick the category (picker with inline
            // creation; the tree's current category is
            // preselected). Cancelling the picker cancels the add.
            let picked_category = {
                let st = state_add.borrow();
                let current = selected_category(&st).unwrap_or_default();
                match st.db.as_ref() {
                    Some(db) => dialogs::show_category_picker(
                        &frame_add,
                        db,
                        &current,
                        "Choose Category",
                        "Category for the new feed:",
                    ),
                    None => Some(String::new()),
                }
            };
            restore_focus(&state_add.borrow(), &tree_add, &list_add);
            let Some(picked_category) = picked_category else {
                frame_add.set_status_text("Add cancelled", 0);
                return;
            };
            std::thread::spawn(move || {
                // One worker subscribe path: discovery chain
                // (social transforms, listing sources, direct
                // feed, HTML alternates), row insert and the
                // first store all happen on this thread.
                let category = Some(picked_category.as_str()).filter(|c| !c.trim().is_empty());
                let result = subscribe_on_worker(&url, category);
                let _ = tx.send(RefreshMsg::Added(url, result, Some(picked_category)));
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
        let tx_rm = tx.clone();
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
            {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if blindrss_core::provider::active_name(&cfg) != "local" {
                    let tx = tx_rm.clone();
                    let feed2 = feed.clone();
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                        let _ = tx.send(RefreshMsg::ProviderAction(
                            provider
                                .remove_feed(&feed2)
                                .map(|_| format!("Removed {}", feed2.title)),
                        ));
                    });
                    return;
                }
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
        let copy_action = |kind: &'static str| -> Rc<dyn Fn()> {
            let state = state.clone();
            Rc::new(move || {
                let st = state.borrow();
                let indices = selected_article_indices(&list, &st);
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let parts: Vec<String> = indices
                    .iter()
                    .filter_map(|&i| {
                        // Copy Text needs bodies; list rows are
                        // light, so resolve this row's body first.
                        let mut owned = st.articles[i].clone();
                        if kind == "text" {
                            ensure_content(st.db.as_ref(), &mut owned);
                        }
                        let article = &owned;
                        let text = match kind {
                            "url" => article_copy_url(article).to_string(),
                            "media" if has_direct_media_link(article) => {
                                article.media_url.clone().unwrap_or_default()
                            }
                            "media" => String::new(),
                            _ if st.selected_article == Some(i)
                                && !reader.get_value().trim().is_empty() =>
                            {
                                reader.get_value()
                            }
                            _ => {
                                let body = article
                                    .content
                                    .as_deref()
                                    .filter(|v| !v.trim().is_empty())
                                    .or(article.description.as_deref())
                                    .unwrap_or("");
                                let body = blindrss_core::extract::html_to_text_opts(
                                    body,
                                    show_image_alt_for(&cfg, st.db.as_ref(), &article.feed_id),
                                );
                                format!(
                                    "{}\nDate: {}\nAuthor: {}\nLink: {}\n\n{}",
                                    article.title,
                                    article.date.as_deref().unwrap_or(""),
                                    article.author.as_deref().unwrap_or(""),
                                    article.url,
                                    body
                                )
                            }
                        };
                        (!text.trim().is_empty()).then_some(text)
                    })
                    .collect();
                let text = parts.join(if kind == "text" { "\n\n---\n\n" } else { "\n" });
                if text.is_empty() {
                    frame.set_status_text("No selected article content to copy", 0);
                } else {
                    let ok = Clipboard::get().set_text(&text);
                    frame.set_status_text(if ok { "Copied" } else { "Copy failed" }, 0);
                }
            })
        };
        let act_copy_url = copy_action("url");
        let act_copy_media = copy_action("media");
        let act_copy_text = copy_action("text");
        let act_open_browser: Rc<dyn Fn()> = {
            let state = state.clone();
            Rc::new(move || {
                let st = state.borrow();
                if let Some(article) = st.selected_article.and_then(|i| st.articles.get(i)) {
                    open_article_link(&frame, &article.url);
                }
            })
        };
        let act_edit_selected: Rc<dyn Fn()> = {
            let state = state.clone();
            let feed = act_edit_feed.clone();
            let category = act_edit_category.clone();
            let smart = act_edit_sf.clone();
            Rc::new(move || {
                let selected = state.borrow().selected_feed.clone().unwrap_or_default();
                if selected.starts_with("cat:") {
                    category();
                } else if selected.starts_with("smart:") {
                    smart();
                } else {
                    feed();
                }
            })
        };
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
                let mut st = state_sr.borrow_mut();
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let seconds = cfg
                    .get_i64("refresh_stop_pause_seconds", 300)
                    .clamp(0, 86_400);
                st.refresh_paused_until = Some(
                    std::time::Instant::now() + std::time::Duration::from_secs(seconds as u64),
                );
                if st.refreshing {
                    st.cancel_refresh
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    frame_sr.set_status_text("Stopping refresh...", 0);
                } else {
                    frame_sr.set_status_text("Automatic refresh paused", 0);
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
                    st.refresh_paused_until = None;
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
                spawn_refresh(feeds, tx_rc.clone(), cancel, stopped, true);
            })
        };

        // New Category / Delete Category (tree hierarchy ops).
        let act_add_category: Rc<dyn Fn()> = {
            let state_ac = state.clone();
            let frame_ac = frame;
            let tx_ac = tx.clone();
            Rc::new(move || {
                // Uncategorized is not a parent: a category created
                // "under" it lands at the root, as in Python.
                let parent = selected_category(&state_ac.borrow())
                    .filter(|p| !is_uncategorized_cat(Some(p)));
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
                    Some(p) => blindrss_core::make_category_path(Some(p), name.trim()),
                    None => blindrss_core::sanitize_category_leaf(name.trim()),
                };
                let st = state_ac.borrow();
                match st.db.as_ref().map(|db| db.ensure_category_path(&full)) {
                    Some(Ok(_)) => {
                        drop(st);
                        mirror_hosted_category_create(&tx_ac, full.clone());
                        let mut st = state_ac.borrow_mut();
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
            let tx_dc = tx.clone();
            Rc::new(move || {
                let Some(path) = selected_category(&state_dc.borrow()) else {
                    frame_dc.set_status_text("Select a category in the tree first", 0);
                    return;
                };
                if is_protected_uncategorized(Some(&path)) {
                    frame_dc.set_status_text("The Uncategorized folder cannot be removed.", 0);
                    return;
                }
                let dlg = MessageDialog::builder(
                    &frame_dc,
                    &format!(
                        "Delete category {path}? Its feeds move to Uncategorized and its subcategories move up to the parent category."
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
                // Snapshot titles first: the global delete
                // target remap needs the old subtree, and whether
                // each descendant landed on its shortened path or
                // (collision fallback) on Uncategorized.
                let titles_before: Vec<String> = state_dc
                    .borrow()
                    .db
                    .as_ref()
                    .and_then(|db| db.categories().ok())
                    .map(|cats| cats.into_iter().map(|c| c.title).collect())
                    .unwrap_or_default();
                let mut st = state_dc.borrow_mut();
                let deleted = st.db.as_ref().map(|db| db.delete_category_path(&path));
                if matches!(deleted, Some(Ok(true))) {
                    let titles_after: Vec<String> = st
                        .db
                        .as_ref()
                        .and_then(|db| db.categories().ok())
                        .map(|cats| cats.into_iter().map(|c| c.title).collect())
                        .unwrap_or_default();
                    let prefix = format!("{path}{}", blindrss_core::CATEGORY_PATH_SEP);
                    let parent_prefix = blindrss_core::category_parent_path(&path)
                        .map(|p| format!("{p}{}", blindrss_core::CATEGORY_PATH_SEP))
                        .unwrap_or_default();
                    let mut mapping = vec![(path.clone(), "Uncategorized".to_string())];
                    for t in titles_before.iter().filter(|t| t.starts_with(&prefix)) {
                        let shortened = format!("{parent_prefix}{}", &t[prefix.len()..]);
                        mapping.push((
                            t.clone(),
                            if titles_after.contains(&shortened) {
                                shortened
                            } else {
                                "Uncategorized".to_string()
                            },
                        ));
                    }
                    drop(st);
                    remap_global_delete_behavior(&mapping);
                    mirror_hosted_category(&tx_dc, None, Some(path.clone()));
                    st = state_dc.borrow_mut();
                }
                match deleted {
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

        // Notifications for This Feed: toggle the selected feed
        // in the notification exclusion list (the gate itself is
        // in refresh_side_effects).
        let act_feed_notif: Rc<dyn Fn()> = {
            let state_nt = state.clone();
            let frame_nt = frame;
            Rc::new(move || {
                let Some(feed_id) = state_nt.borrow().selected_feed.clone() else {
                    frame_nt.set_status_text("Select a feed in the tree first", 0);
                    return;
                };
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let excluded = notification_excluded_ids(&cfg).contains(&feed_id);
                set_notification_excluded(&feed_id, !excluded);
                frame_nt.set_status_text(
                    if excluded {
                        "Notifications on for this feed"
                    } else {
                        "Notifications off for this feed"
                    },
                    0,
                );
            })
        };

        // Image Alt Text submenu actions: per-feed override
        // (None = use the global setting, Some = always/never).
        let make_imgalt = |mode: Option<bool>| -> Rc<dyn Fn()> {
            let state_ia = state.clone();
            let frame_ia = frame;
            let rich_view_ia = rich_view.clone();
            let rich_loading_ia = rich_loading.clone();
            let tx_ia = tx.clone();
            Rc::new(move || {
                let (feed_id, done) = {
                    let st = state_ia.borrow();
                    let feed_id = st.selected_feed.clone();
                    let done = match (&feed_id, &st.db) {
                        (Some(id), Some(db)) => db.set_feed_show_images(id, mode).is_ok(),
                        _ => false,
                    };
                    (feed_id, done)
                };
                if feed_id.is_none() {
                    frame_ia.set_status_text("Select a feed in the tree first", 0);
                } else if done {
                    frame_ia.set_status_text(
                        match mode {
                            None => "Image alt text: using the global setting for this feed",
                            Some(true) => "Image alt text: always for this feed",
                            Some(false) => "Image alt text: never for this feed",
                        },
                        0,
                    );
                    // Audit 8 G16: the change re-renders the
                    // current article at once (Python
                    // `_update_content_view`) when it belongs
                    // to this feed — no reselection needed.
                    let selected_feed = {
                        let st = state_ia.borrow();
                        st.selected_article
                            .and_then(|i| st.articles.get(i))
                            .map(|a| a.feed_id.clone())
                    };
                    if selected_feed.as_deref() == feed_id.as_deref() {
                        rerender_current_article(
                            &state_ia,
                            &frame_ia,
                            &reader,
                            &rich_view_ia,
                            &rich_loading_ia,
                            &tx_ia,
                        );
                    }
                }
            })
        };
        let act_imgalt_default = make_imgalt(None);
        let act_imgalt_always = make_imgalt(Some(true));
        let act_imgalt_never = make_imgalt(Some(false));

        // Delete Category and Its Feeds (Python
        // on_delete_category_with_feeds): the category, its
        // subcategories, and every feed in them are removed.
        let act_delete_category_feeds: Rc<dyn Fn()> = {
            let state_df = state.clone();
            let frame_df = frame;
            let tx_df = tx.clone();
            Rc::new(move || {
                let Some(path) = selected_category(&state_df.borrow()) else {
                    frame_df.set_status_text("Select a category in the tree first", 0);
                    return;
                };
                if is_protected_uncategorized(Some(&path)) {
                    frame_df.set_status_text("The Uncategorized folder cannot be removed.", 0);
                    return;
                }
                let (feeds, subcats) = {
                    let st = state_df.borrow();
                    let feeds = st
                        .db
                        .as_ref()
                        .and_then(|db| db.feeds_in_category_tree(&path).ok())
                        .unwrap_or_default();
                    let prefix = format!("{path}{}", blindrss_core::CATEGORY_PATH_SEP);
                    let subcats = st
                        .db
                        .as_ref()
                        .and_then(|db| db.categories().ok())
                        .map(|cats| {
                            cats.iter()
                                .filter(|c| c.title.starts_with(&prefix))
                                .map(|c| c.title.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    (feeds, subcats)
                };
                let n = feeds.len();
                let sub_note = if subcats.is_empty() {
                    ""
                } else {
                    " (including subcategories)"
                };
                let dlg = MessageDialog::builder(
                    &frame_df,
                    &format!(
                        "Delete category '{path}'{sub_note} and its {n} feed(s)?\n\nThis will remove the feeds and their articles."
                    ),
                    "Delete Category and Its Feeds",
                )
                .with_style(MessageDialogStyle::YesNo)
                .build();
                let rc = dlg.show_modal();
                restore_focus(&state_df.borrow(), &tree, &list);
                if rc != ID_YES {
                    return;
                }
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if blindrss_core::provider::active_name(&cfg) != "local" {
                    // Remote provider: remove the feeds on a
                    // worker (each removal also drops the local
                    // mirror row), then the completion reload
                    // refreshes the tree.
                    let tx2 = tx_df.clone();
                    let feeds2 = feeds.clone();
                    let path2 = path.clone();
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                        let mut failed = 0usize;
                        for f in &feeds2 {
                            if provider.remove_feed(f).is_err() {
                                failed += 1;
                            }
                        }
                        let _ = tx2.send(RefreshMsg::ProviderAction(if failed == 0 {
                            Ok(format!(
                                "Deleted category {path2} and its {} feed(s)",
                                feeds2.len()
                            ))
                        } else {
                            Err(format!("{failed} feed(s) could not be removed"))
                        }));
                    });
                }
                // Local rows: feeds (local provider only — the
                // remote worker handles its own), then the
                // category tree, mirrored to Miniflux deepest
                // first like Python.
                let mut st = state_df.borrow_mut();
                if let Some(db) = st.db.as_ref() {
                    if blindrss_core::provider::active_name(&cfg) == "local" {
                        for f in &feeds {
                            let _ = db.remove_feed(&f.id);
                        }
                    }
                    let mut deepest_first = subcats.clone();
                    deepest_first.sort_by_key(|t| std::cmp::Reverse(t.len()));
                    for sub in &deepest_first {
                        mirror_hosted_category(&tx_df, None, Some(sub.clone()));
                    }
                    let _ = db.delete_category_tree(&path);
                    mirror_hosted_category(&tx_df, None, Some(path.clone()));
                    st.feeds = db.feeds().unwrap_or_default();
                }
                st.selected_feed = None;
                rebuild_tree(&mut st, &tree);
                refill_articles(&mut st, &list);
                frame_df
                    .set_status_text(&format!("Category {path} and its {n} feed(s) deleted"), 0);
            })
        };

        // Show Search Field (Python _set_search_visible /
        // view.toggle_search): View-menu checkable; persists
        // show_search_field and shows/hides the box live.
        let act_toggle_search: Rc<dyn Fn()> = {
            let frame_ts = frame;
            Rc::new(move || {
                let mut cfg =
                    blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let show = !cfg.get_bool("show_search_field", true);
                cfg.set_path("show_search_field", serde_json::json!(show));
                let _ = cfg.save(&config_path());
                search_box.show(show);
                frame_ts.layout();
                if let Some(bar) = frame_ts.get_menu_bar() {
                    bar.check_item(M_TOGGLE_SEARCH, show);
                }
            })
        };

        let act_rich_view: Rc<dyn Fn()> = {
            let state = state.clone();
            let rich_view = rich_view.clone();
            let loading = rich_loading.clone();
            let tx = tx.clone();
            Rc::new(move || {
                let mut cfg =
                    blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let show = !cfg.get_bool("full_text_rich_view", false);
                if show && rich_view.is_none() {
                    frame.set_status_text("HTML reader is unavailable on this system", 0);
                    return;
                }
                cfg.set_path("full_text_rich_view", serde_json::json!(show));
                if let Err(e) = cfg.save(&config_path()) {
                    frame.set_status_text(&format!("Could not save reader mode: {e}"), 0);
                    return;
                }
                reader.show(!show);
                if let Some(view) = rich_view.as_ref() {
                    view.show(show);
                    if show
                        && let Some(article) = state
                            .borrow()
                            .selected_article
                            .and_then(|i| state.borrow().articles.get(i).cloned())
                    {
                        let body = article
                            .content
                            .as_deref()
                            .or(article.description.as_deref())
                            .unwrap_or("");
                        if let Some(html) = blindrss_core::article_html::rich_article_from_feed(
                            &article.url,
                            body,
                            &article.title,
                            article.author.as_deref().unwrap_or(""),
                            article.date.as_deref().unwrap_or(""),
                            "",
                            "",
                            &ui_language(),
                        ) {
                            loading.set(true);
                            view.set_page(&rich_document(&html), &article.url);
                        }
                        spawn_rich_fetch(&tx, &article);
                    }
                }
                right.layout();
                if let Some(bar) = frame.get_menu_bar() {
                    bar.check_item(M_RICH_VIEW, show);
                }
            })
        };

        // View Description (Python on_view_feed_description):
        // the selected article's feed-provided description in
        // its own window.
        let act_view_desc: Rc<dyn Fn()> = {
            let state_vd = state.clone();
            let frame_vd = frame;
            Rc::new(move || {
                let st = state_vd.borrow();
                let Some(mut a) = st
                    .selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
                else {
                    frame_vd.set_status_text("No article selected", 0);
                    return;
                };
                // The description view falls back to the body;
                // list rows are light, so resolve it.
                ensure_content(st.db.as_ref(), &mut a);
                let raw = a
                    .description
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .or_else(|| a.content.clone())
                    .unwrap_or_default();
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let keep_alt = show_image_alt_for(&cfg, st.db.as_ref(), &a.feed_id);
                let text = if raw.trim().is_empty() {
                    "No feed description is available for this item.".to_string()
                } else {
                    blindrss_core::extract::html_to_text_opts(&raw, keep_alt)
                };
                dialogs::show_article_window(
                    "Feed Description",
                    &format!("{}\n\n{}", a.title, text),
                );
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
        let act_sort_desc = sort_action("description", "Sort by Description");
        let act_sort_status = sort_action("status", "Sort by Status");
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

        // Batch 41a actions for the Python-shaped menus.
        // Player-menu transport routes through the player
        // window's command dispatch, exactly like the shortcut
        // handler; Play/Pause falls back to playing the selected
        // article when no player is open (the shortcut path's
        // behaviour). The seek/volume command names still need
        // arms in player.rs — dispatch returns false until then
        // and the status line says so.
        let frame_pc = frame;
        let player_cmd = move |cmd: &'static str, label: &'static str| -> Rc<dyn Fn()> {
            Rc::new(move || {
                if !player::dispatch_command(cmd) {
                    frame_pc.set_status_text(&format!("{label}: no player is open"), 0);
                }
            })
        };
        let act_player_show_hide = player_cmd("player.show_hide", "Show/Hide Player");
        let act_player_stop = player_cmd("player.stop", "Stop");
        let act_player_rewind = player_cmd("player.rewind", "Rewind");
        let act_player_ff = player_cmd("player.forward", "Fast Forward");
        let act_player_vol_up = player_cmd("player.volume_up", "Volume Up");
        let act_player_vol_down = player_cmd("player.volume_down", "Volume Down");
        let act_speed_up = player_cmd("speed.up", "Increase Speed");
        let act_speed_down = player_cmd("speed.down", "Decrease Speed");
        let act_speed_reset = player_cmd("speed.reset", "Normal Speed (1x)");
        let act_player_chapters = player_cmd("player.chapters", "Chapters");
        let act_player_play_pause: Rc<dyn Fn()> = {
            let play = act_play.clone();
            Rc::new(move || {
                if !player::dispatch_command("player.play_pause") {
                    play();
                }
            })
        };
        let act_announce_version: Rc<dyn Fn()> = Rc::new(move || {
            announce(
                "general",
                &format!("BlindRSS {}", env!("CARGO_PKG_VERSION")),
            );
        });
        let act_youtube_account: Rc<dyn Fn()> = {
            let state_ya = state.clone();
            let frame_ya = frame;
            let tree_ya = tree;
            let list_ya = list;
            Rc::new(move || {
                dialogs::show_youtube_signin(&frame_ya, &config_path());
                restore_focus(&state_ya.borrow(), &tree_ya, &list_ya);
            })
        };
        // Selection-aware mark read/unread (Python's list
        // context menu): set, not toggle.
        let mark_sel = |read: bool| -> Rc<dyn Fn()> {
            let state_ms = state.clone();
            let frame_ms = frame;
            let list_ms = list;
            Rc::new(move || {
                let mut st = state_ms.borrow_mut();
                let indices = selected_article_indices(&list_ms, &st);
                if indices.is_empty() {
                    frame_ms.set_status_text("No article selected", 0);
                    return;
                }
                let articles: Vec<Article> =
                    indices.iter().map(|&i| st.articles[i].clone()).collect();
                for (i, article) in indices.iter().zip(&articles) {
                    if let Some(db) = &st.db {
                        mark_article_read(db, article, read);
                    }
                    st.articles[*i].is_read = read;
                }
                refill_articles(&mut st, &list_ms);
                frame_ms.set_status_text(
                    &format!(
                        "Marked {} article(s) {}",
                        articles.len(),
                        if read { "read" } else { "unread" }
                    ),
                    0,
                );
                drop(st);
                for article in articles {
                    propagate_read_state(&article, read);
                }
            })
        };
        let act_mark_read_sel = mark_sel(true);
        let act_mark_unread_sel = mark_sel(false);
        // Selection-aware play-queue add/remove (Python's list
        // context menu): playable selections only, mirroring
        // act_toggle_queue's entry shape.
        let queue_sel = |add: bool| -> Rc<dyn Fn()> {
            let state_qs = state.clone();
            let frame_qs = frame;
            let list_qs = list;
            Rc::new(move || {
                let st = state_qs.borrow();
                let indices = selected_article_indices(&list_qs, &st);
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let mut entries = dialogs::load_queue(&config_path());
                let mut changed = 0usize;
                for i in indices {
                    let article = &st.articles[i];
                    let playable = blindrss_core::download::downloaded_media_path(&cfg, article)
                        .is_some()
                        || article.media_url.as_deref().is_some_and(|u| {
                            !u.is_empty()
                                && blindrss_core::media::is_playable_media(
                                    u,
                                    article.media_type.as_deref(),
                                )
                        })
                        || blindrss_core::media::is_ytdlp_supported(&article.url);
                    if !playable {
                        continue;
                    }
                    let pos = entries.iter().position(|e| {
                        e.get("article_id").and_then(|v| v.as_str()) == Some(article.id.as_str())
                    });
                    if add && pos.is_none() {
                        let feed_title = st
                            .feeds
                            .iter()
                            .find(|f| f.id == article.feed_id)
                            .map(|f| f.title.clone())
                            .unwrap_or_default();
                        entries.push(serde_json::json!({
                            "article_id": article.id,
                            "media_url": article.media_url.clone().unwrap_or_else(|| article.url.clone()),
                            "use_ytdlp": article.media_url.is_none(),
                            "title": article.title,
                            "media_type": article.media_type.clone().unwrap_or_default(),
                            "feed_id": article.feed_id,
                            "feed_title": feed_title,
                        }));
                        changed += 1;
                    } else if !add && let Some(pos) = pos {
                        entries.remove(pos);
                        changed += 1;
                    }
                }
                if changed > 0 {
                    dialogs::save_queue(&config_path(), &entries);
                }
                frame_qs.set_status_text(
                    &format!(
                        "{} {changed} article(s) {} the play queue",
                        if add { "Added" } else { "Removed" },
                        if add { "to" } else { "from" }
                    ),
                    0,
                );
            })
        };
        let act_queue_add_sel = queue_sel(true);
        let act_queue_remove_sel = queue_sel(false);
        // Detect Audio (Python's on_detect_audio): scan the
        // selected article's page on a worker; the drain
        // persists the hit via RefreshMsg::AudioDetected.
        let act_detect_audio: Rc<dyn Fn()> = {
            let state_da = state.clone();
            let frame_da = frame;
            let tx_da = tx.clone();
            Rc::new(move || {
                let article = {
                    let st = state_da.borrow();
                    st.selected_article
                        .and_then(|i| st.articles.get(i))
                        .cloned()
                };
                let Some(article) = article else {
                    frame_da.set_status_text("No article selected", 0);
                    return;
                };
                if article.media_url.as_deref().is_some_and(|u| !u.is_empty()) {
                    frame_da.set_status_text("Article already has media", 0);
                    return;
                }
                if article.url.is_empty() {
                    frame_da.set_status_text("Article has no link to scan", 0);
                    return;
                }
                frame_da.set_status_text("Scanning for audio...", 0);
                let tx = tx_da.clone();
                std::thread::spawn(move || {
                    let found = blindrss_core::extract::fetch_html(&article.url)
                        .ok()
                        .and_then(|html| {
                            blindrss_core::media::detect_media_in_page(&html, &article.url)
                        });
                    let _ = tx.send(RefreshMsg::AudioDetected(article.id.clone(), found));
                });
            })
        };
        // Copy Image Link (Python's on_copy_image_link): the
        // first image URL in the selected article's content.
        let act_copy_image: Rc<dyn Fn()> = {
            let state_ci = state.clone();
            let frame_ci = frame;
            Rc::new(move || {
                let st = state_ci.borrow();
                let Some(article) = st.selected_article.and_then(|i| st.articles.get(i)) else {
                    frame_ci.set_status_text("No article selected", 0);
                    return;
                };
                match first_image_url(article.content.as_deref()) {
                    Some(url) => {
                        let ok = Clipboard::get().set_text(&url);
                        frame_ci.set_status_text(
                            if ok {
                                "Image link copied"
                            } else {
                                "Copy failed"
                            },
                            0,
                        );
                    }
                    None => frame_ci.set_status_text("No image in this article", 0),
                }
            })
        };
        // Reset Title to Feed Default (Python's tree context
        // item): re-fetch the feed document and restore its own
        // title, clearing the custom flag.
        let act_reset_title: Rc<dyn Fn()> = {
            let state_rt = state.clone();
            let frame_rt = frame;
            let tree_rt = tree;
            Rc::new(move || {
                let feed = {
                    let st = state_rt.borrow();
                    st.selected_feed
                        .as_ref()
                        .and_then(|id| st.feeds.iter().find(|f| &f.id == id).cloned())
                };
                let Some(feed) = feed else {
                    frame_rt.set_status_text("Select a feed in the tree first", 0);
                    return;
                };
                match blindrss_core::feeds::fetch_and_parse(&feed.url) {
                    Ok(parsed) => {
                        let mut st = state_rt.borrow_mut();
                        if let Some(db) = &st.db {
                            let _ = db.reset_feed_title(&feed.id, &parsed.title);
                            st.feeds = db.feeds().unwrap_or_default();
                        }
                        rebuild_tree(&mut st, &tree_rt);
                        frame_rt.set_status_text("Feed title reset", 0);
                    }
                    Err(e) => frame_rt.set_status_text(&format!("Reset title: {e}"), 0),
                }
            })
        };
        // Chapter Links submenu slots: the list context menu
        // stores the selected article's chapter hrefs here.
        let chapter_link_urls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let act_chapter_links: Vec<Rc<dyn Fn()>> = (0..M_CHAPTER_LINK_SLOTS)
            .map(|slot| {
                let urls = chapter_link_urls.clone();
                Rc::new(move || {
                    if let Some(url) = urls.borrow().get(slot as usize).cloned() {
                        open_article_link(&frame, &url);
                    }
                }) as Rc<dyn Fn()>
            })
            .collect();

        // Menu bar rebuilt to Python's exact map (Batch 41a /
        // Audit 6 G1.1; authority gui/mainframe.py init_menus +
        // gui/menu_mnemonics.py labels): File / Feeds / Edit /
        // View / Player / Tools / Help, item for item. Actions
        // Python keeps out of the bar (feed/category editing,
        // article operations) live in the context menus below,
        // as in Python; their handlers are unchanged.
        let file_menu = Menu::builder()
            .append_item(
                M_OPEN_ARTICLE,
                "&Open Article...",
                "Fetch and read a web article by URL",
            )
            .append_item(
                M_OPEN_MEDIA,
                "Open Media &URL...",
                "Play a media URL in the player",
            )
            .append_item(
                M_MARKALL,
                "Mark All Items as &Read",
                "Mark every shown article read",
            )
            .append_separator()
            .append_item(M_ADD_CAT, "Add &Category", "Add a new category")
            .append_item(
                M_DEL_CAT,
                "Remove C&ategory",
                "Remove the selected category (feeds move up)",
            )
            .append_separator()
            .append_item(M_BACKUP, "&Back Up Data...", "Save a backup ZIP")
            .append_item(M_RESTORE, "&Restore Data...", "Restore from a backup ZIP")
            .append_separator()
            .append_item(
                M_PERSISTENT,
                "Configure &Persistent Search...",
                "Saved searches across all articles",
            );
        // Add Shortcuts (Batch 42): Python shows the item on
        // every platform except macOS, in File after Configure
        // Persistent Search.
        #[cfg(not(target_os = "macos"))]
        let file_menu = file_menu.append_item(
            M_ADD_SHORTCUTS,
            "Add &Shortcuts...",
            "Add BlindRSS shortcuts to the desktop, Start menu or taskbar",
        );
        let file_menu = file_menu
            .append_separator()
            .append_item(M_EXIT, "E&xit", "Exit application")
            .build();
        let edit_menu = Menu::builder()
            .append_item(5031, "Cu&t\tCtrl+X", "Cut the selection")
            .append_item(5032, "&Copy\tCtrl+C", "Copy the selection")
            .append_item(5033, "&Paste\tCtrl+V", "Paste from the clipboard")
            .append_item(5037, "Select &All\tCtrl+A", "Select all")
            .build();
        let feeds_menu = Menu::builder()
            .append_item(M_ADD_FEED, "&Add Feed", "Add a new RSS feed")
            .append_item(
                M_FEED_SEARCH,
                "Detect Feeds on &Page...",
                "Find feeds on a web page",
            )
            .append_item(M_REMOVE_FEED, "&Remove Feed", "Remove selected feed")
            .append_item(M_REFRESH, "&Refresh Feeds", "Refresh all feeds")
            .append_item(M_STOP_REFRESH, "S&top Refresh", "Stop refreshing")
            .append_item(
                M_FEED_ERRORS,
                "View Feed &Errors...",
                "Feeds whose latest refresh failed",
            )
            .append_separator()
            .append_item(M_IMPORT_OPML, "&Import OPML...", "Import feeds from OPML")
            .append_item(
                M_TAKEOUT,
                "Import &YouTube Takeout...",
                "Import subscriptions from a Takeout ZIP",
            )
            .append_item(M_EXPORT_OPML, "E&xport OPML...", "Export feeds to OPML")
            .build();
        // View menu (Python): Show Search Field, Rich
        // Full-Text View, Article Filter submenu, the
        // Accessible Browser item (kept verbatim; Batch 40
        // removes it on rebase), Show/Hide Player, Sort By
        // submenu. Submenus are appended after the builder
        // items so the Python order is preserved exactly.
        let view_menu = Menu::builder()
            .append_check_item(
                M_TOGGLE_SEARCH,
                "Show &Search Field",
                "Show or hide the search field",
            )
            .append_check_item(
                M_RICH_VIEW,
                "&Rich Full-Text View",
                "Switch the reader between plain full text and the rich HTML view",
            )
            .build();
        {
            let filter_menu = Menu::builder()
                .append_radio_item(
                    M_FILTER_ALL,
                    "&All Articles",
                    "Show read and unread articles in all views",
                )
                .append_radio_item(
                    M_FILTER_UNREAD,
                    "&Unread Only",
                    "Show only unread articles in all views",
                )
                .append_radio_item(
                    M_FILTER_READ,
                    "&Read Only",
                    "Show only read articles in all views",
                )
                .append_separator()
                .append_radio_item(
                    M_MEDIA_ALL,
                    "Media and &Non-media",
                    "Show articles whether or not they have media",
                )
                .append_radio_item(
                    M_MEDIA_WITH,
                    "With &Media Only",
                    "Show only articles that have playable media",
                )
                .append_radio_item(
                    M_MEDIA_WITHOUT,
                    "Wit&hout Media Only",
                    "Show only articles that have no playable media",
                )
                .build();
            view_menu.append_submenu(
                filter_menu,
                "Article &Filter",
                "Filter all views by read status and media",
            );
        }
        view_menu.append(
            M_PLAYER_SHOW_HIDE,
            "Show/Hide &Player",
            "Show or hide the media player window",
            ItemKind::Normal,
        );
        view_menu.append_separator();
        {
            let sort_menu = Menu::builder()
                .append_radio_item(M_SORT_DATE, "&Date", "Sort articles by date")
                .append_radio_item(M_SORT_NAME, "&Name", "Sort articles by name")
                .append_radio_item(M_SORT_AUTHOR, "A&uthor", "Sort articles by author")
                .append_radio_item(M_SORT_DESC, "De&scription", "Sort articles by description")
                .append_radio_item(M_SORT_FEED, "&Feed", "Sort articles by feed")
                .append_radio_item(M_SORT_STATUS, "S&tatus", "Sort articles by status")
                .append_separator()
                .append_check_item(
                    M_SORT_DIR,
                    "&Ascending",
                    "Sort in ascending order (default is descending by date)",
                )
                .build();
            view_menu.append_submenu(sort_menu, "Sort &By", "");
        }
        // Player menu (Python): transport for the player
        // window, Play Queue, speed submenu, Equalizer,
        // seek/volume with their shortcut text in the label
        // (Python deliberately uses no \t accelerators here),
        // and a Chapters submenu whose only static entry is
        // Show Chapters... — Python fills the rest with the
        // current track's chapters, which live in the player
        // window in Rust and cannot be listed from here.
        let player_menu = Menu::builder()
            .append_item(
                M_PLAYER_SHOW_HIDE,
                "Show/Hide Player",
                "Show or hide the media player window",
            )
            .append_separator()
            .append_item(M_PLAYER_PLAY_PAUSE, "Play/Pause", "Toggle play/pause")
            .append_item(M_PLAYER_STOP, "Stop", "Stop playback")
            .append_separator()
            .append_item(
                M_QUEUE_OPEN,
                "Play &Queue...",
                "View and edit the play queue",
            )
            .append_item(
                M_QUEUE_NEXT,
                "Play Next in Queue",
                "Play the next queue entry",
            )
            .append_item(
                M_QUEUE_PREV,
                "Play Previous in Queue",
                "Play the previous queue entry",
            )
            .append_separator()
            .build();
        {
            let speed_menu = Menu::builder()
                .append_item(M_SPEED_UP, "Increase Speed", "Play faster")
                .append_item(M_SPEED_DOWN, "Decrease Speed", "Play slower")
                .append_item(M_SPEED_RESET, "Normal Speed (1x)", "Reset to normal speed")
                .build();
            player_menu.append_submenu(speed_menu, "Playback &Speed", "");
        }
        player_menu.append(
            M_EQUALIZER,
            "&Equalizer...",
            "Adjust the equalizer",
            ItemKind::Normal,
        );
        player_menu.append_separator();
        player_menu.append(
            M_PLAYER_REWIND,
            "Rewind (Ctrl+Left)",
            "Rewind",
            ItemKind::Normal,
        );
        player_menu.append(
            M_PLAYER_FF,
            "Fast Forward (Ctrl+Right)",
            "Fast forward",
            ItemKind::Normal,
        );
        player_menu.append_separator();
        player_menu.append(
            M_PLAYER_VOL_UP,
            "Volume Up (Ctrl+Up)",
            "Increase volume",
            ItemKind::Normal,
        );
        player_menu.append(
            M_PLAYER_VOL_DOWN,
            "Volume Down (Ctrl+Down)",
            "Decrease volume",
            ItemKind::Normal,
        );
        player_menu.append_separator();
        {
            let chapters_menu = Menu::builder()
                .append_item(M_PLAYER_CHAPTERS, "Show Chapters...", "Show chapter list")
                .build();
            player_menu.append_submenu(chapters_menu, "Chapters", "");
        }
        // (The pre-41a View menu's Find items stay available
        // through the reader's Ctrl+F / F3 handling, as in
        // Python, which has no Find entries in its View menu.)
        let tools_menu = Menu::builder()
            .append_item(
                M_FEED_SEARCH,
                "Find a &Podcast or RSS Feed...",
                "Search for a podcast or RSS feed to subscribe to",
            )
            .append_item(
                M_PODCAST_ARCHIVE,
                "Podcast &Archive...",
                "Recover old podcast episodes from the Wayback Machine",
            )
            .append_item(
                M_VIDEO_SEARCH,
                "&Video Search...",
                "Search videos across sites with yt-dlp",
            )
            .append_separator()
            .append_item(
                M_FILTER_RULES,
                "Filter &Rules...",
                "Rules that act on new articles",
            )
            .append_item(
                M_IMPORT_COOKIES,
                "Import Site &Cookies...",
                "Import browser cookies so challenge-protected sites can be read",
            )
            .append_item(
                M_YOUTUBE_ACCOUNT,
                "YouTube &Account...",
                "Sign in to a YouTube account",
            )
            .append_separator()
            .append_item(
                M_KEYS,
                "&Keyboard Shortcuts...",
                "View and edit keyboard shortcuts",
            )
            .append_item(M_SETTINGS, "&Settings...", "Configure application")
            .build();
        let help_menu = Menu::builder()
            .append_item(M_HELP, "&User Guide", "Open the BlindRSS user guide")
            .append_separator()
            .append_item(
                M_UPDATE,
                "Check for &Updates...",
                "Check GitHub for a newer release",
            )
            .append_item(
                M_ANNOUNCE_VERSION,
                "Announce &Version",
                "Announce the version",
            )
            .append_item(M_ABOUT, "&About", "About BlindRSS")
            .build();
        let menu_bar = MenuBar::builder()
            .append(file_menu, "&File")
            .append(feeds_menu, "Fee&ds")
            .append(edit_menu, "&Edit")
            .append(view_menu, "&View")
            .append(player_menu, "&Player")
            .append(tools_menu, "&Tools")
            .append(help_menu, "&Help")
            .build();
        frame.set_menu_bar(menu_bar);
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if let Some(bar) = frame.get_menu_bar() {
                bar.check_item(M_TOGGLE_SEARCH, cfg.get_bool("show_search_field", true));
                bar.check_item(M_RICH_VIEW, cfg.get_bool("full_text_rich_view", false));
                // Filter/sort radio + check state (Python checks
                // the current modes when the bar is built; the
                // Rust state starts at the defaults, matching a
                // fresh Python start).
                bar.check_item(M_FILTER_ALL, true);
                bar.check_item(M_MEDIA_ALL, true);
                bar.check_item(
                    M_SORT_DATE,
                    cfg.get_str("article_sort_by", "date") == "date",
                );
                bar.check_item(
                    M_SORT_NAME,
                    cfg.get_str("article_sort_by", "date") == "name",
                );
                bar.check_item(
                    M_SORT_AUTHOR,
                    cfg.get_str("article_sort_by", "date") == "author",
                );
                bar.check_item(
                    M_SORT_DESC,
                    cfg.get_str("article_sort_by", "date") == "description",
                );
                bar.check_item(
                    M_SORT_FEED,
                    cfg.get_str("article_sort_by", "date") == "feed",
                );
                bar.check_item(
                    M_SORT_STATUS,
                    cfg.get_str("article_sort_by", "date") == "status",
                );
                bar.check_item(M_SORT_DIR, cfg.get_bool("article_sort_ascending", false));
            }
        }
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
            (M_DEL_CAT_FEEDS, act_delete_category_feeds.clone()),
            (M_FEED_NOTIF, act_feed_notif.clone()),
            (M_IMGALT_DEFAULT, act_imgalt_default.clone()),
            (M_IMGALT_ALWAYS, act_imgalt_always.clone()),
            (M_IMGALT_NEVER, act_imgalt_never.clone()),
            (M_VIEW_DESC, act_view_desc.clone()),
            (M_TOGGLE_SEARCH, act_toggle_search.clone()),
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
            (M_HISTORY, act_history.clone()),
            (M_FAVORITE, act_fav.clone()),
            (M_MARKALL, act_markall.clone()),
            (M_MARKALL_CTX, act_markall_ctx.clone()),
            (M_DELETE_ARTICLE, act_delete.clone()),
            (M_DELETED, act_deleted.clone()),
            (M_PLAY, act_play.clone()),
            (M_DOWNLOAD, act_download.clone()),
            (M_DOWNLOAD_AS, act_download_as.clone()),
            (M_CAST, act_cast.clone()),
            (M_STOP_CAST, act_stop_cast.clone()),
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
            (M_EDIT_SF, act_edit_sf.clone()),
            (M_FILTER_RULES, act_filter_rules.clone()),
            (M_FEED_PROPS, act_feed_props.clone()),
            (M_OPEN_ARTICLE, act_open_article.clone()),
            (M_OPEN_MEDIA, act_open_media.clone()),
            (M_VIDEO_SEARCH, act_video_search.clone()),
            (M_IMPORT_COOKIES, act_import_cookies.clone()),
            (M_PERSISTENT, act_persistent.clone()),
            (M_ADD_SHORTCUTS, act_add_shortcuts.clone()),
            (M_ABOUT, act_about.clone()),
            (M_ARTICLE_WINDOW, act_article_window.clone()),
            (M_PODCAST_ARCHIVE, act_podcast_archive.clone()),
            (M_QUEUE_TOGGLE, act_toggle_queue.clone()),
            (M_QUEUE_OPEN, act_queue_open.clone()),
            (M_QUEUE_NEXT, act_queue_next.clone()),
            (M_QUEUE_PREV, act_queue_prev.clone()),
            (M_FEED_SEARCH, act_feed_search.clone()),
            (M_TRANSLATE, act_translate.clone()),
            (M_COPY_TEXT, act_copy_text.clone()),
            (M_OPEN_BROWSER, act_open_browser.clone()),
            (M_EDIT_SELECTED, act_edit_selected.clone()),
            (M_RICH_VIEW, act_rich_view.clone()),
            (M_SORT_DESC, act_sort_desc.clone()),
            (M_SORT_STATUS, act_sort_status.clone()),
            (M_COPY_URL, act_copy_url.clone()),
            (M_COPY_MEDIA, act_copy_media.clone()),
            (M_COPY_FEED_URL, act_copy_feed_url.clone()),
            // Batch 41a additions.
            (M_PLAYER_SHOW_HIDE, act_player_show_hide.clone()),
            (M_PLAYER_PLAY_PAUSE, act_player_play_pause.clone()),
            (M_PLAYER_STOP, act_player_stop.clone()),
            (M_PLAYER_REWIND, act_player_rewind.clone()),
            (M_PLAYER_FF, act_player_ff.clone()),
            (M_PLAYER_VOL_UP, act_player_vol_up.clone()),
            (M_PLAYER_VOL_DOWN, act_player_vol_down.clone()),
            (M_SPEED_UP, act_speed_up.clone()),
            (M_SPEED_DOWN, act_speed_down.clone()),
            (M_SPEED_RESET, act_speed_reset.clone()),
            (M_PLAYER_CHAPTERS, act_player_chapters.clone()),
            (M_EQUALIZER, act_equalizer.clone()),
            (M_ANNOUNCE_VERSION, act_announce_version.clone()),
            (M_YOUTUBE_ACCOUNT, act_youtube_account.clone()),
            (M_MARK_READ_SEL, act_mark_read_sel.clone()),
            (M_MARK_UNREAD_SEL, act_mark_unread_sel.clone()),
            (M_QUEUE_ADD_SEL, act_queue_add_sel.clone()),
            (M_QUEUE_REMOVE_SEL, act_queue_remove_sel.clone()),
            (M_DETECT_AUDIO, act_detect_audio.clone()),
            (M_COPY_IMAGE, act_copy_image.clone()),
            (M_ACTIVATE, act_activate.clone()),
            (M_RESET_TITLE, act_reset_title.clone()),
            (M_EDIT_CAT, act_edit_category.clone()),
        ];
        // Chapter Links slots (Batch 41a): ids are a contiguous
        // range, appended after the fixed table.
        let mut menu_actions = menu_actions;
        for (slot, action) in act_chapter_links.into_iter().enumerate() {
            menu_actions.push((M_CHAPTER_LINK_BASE + slot as i32, action));
        }
        let frame_menu = frame;
        let quit_menu = quit_flag.clone();
        frame.on_menu(move |ev| {
            let id = ev.get_id();
            if id == M_EXIT {
                quit_menu.set(true);
                frame_menu.close(true);
                return;
            }
            if let Some((_, action)) = menu_actions.iter().find(|(mid, _)| *mid == id) {
                action();
            } else {
                ev.skip(true);
            }
        });

        // Context menus (PARITY 5C): right-click / Menu key on the
        // feed tree and the article list. Selections dispatch through
        // the frame's menu handler above (wx command events from a
        // popup propagate to the frame), so the same actions run.
        let state_ctx_tree = state.clone();
        tree.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            // Tree context menu varies by node kind, matching
            // Python's on_tree_context_menu (Batch 41a / Audit 6
            // G1.2). Rust-only extras Python keeps elsewhere
            // (Feed Properties, Set Feed Category, Rescan
            // Archive, Sync Providers) stay at the end of the
            // relevant variant so no handler loses its surface.
            let selected = state_ctx_tree.borrow().selected_feed.clone();
            let kind = match selected.as_deref() {
                Some(s) if s.starts_with("cat:") => "category",
                Some("favorites:all") => "all",
                Some("smart:") => "smart_root",
                Some(s) if s.starts_with("smart:") => "smart",
                Some(_) => "feed",
                None => "all",
            };
            // Scoped Mark All (Audit 8 G05): the context item
            // dispatches M_MARKALL_CTX, whose handler consumes
            // the (view, prompt) stashed here for THIS node —
            // the main command's global scope is never reused
            // for a node's menu.
            if matches!(kind, "category" | "feed") {
                let feed_title = {
                    let st = state_ctx_tree.borrow();
                    selected.as_ref().and_then(|sel| {
                        st.feeds
                            .iter()
                            .find(|f| &f.id == sel)
                            .map(|f| f.title.clone())
                    })
                };
                let prompt = mark_all_prompt(selected.as_deref(), feed_title.as_deref());
                state_ctx_tree.borrow_mut().ctx_mark = Some((selected.clone(), prompt));
            }
            let mut builder = Menu::builder();
            match kind {
                "category" => {
                    builder = builder
                        .append_item(M_REFRESH_CAT, "Refresh Category", "")
                        .append_item(M_MARKALL_CTX, "Mark All Items as Read", "")
                        .append_item(M_ADD_CAT, "Add Subcategory", "")
                        .append_item(M_EDIT_CAT, "Edit Category", "")
                        .append_item(M_DEL_CAT, "Remove Category", "")
                        .append_item(M_DEL_CAT_FEEDS, "Delete Category and Feeds", "")
                        .append_item(M_IMPORT_HERE, "Import OPML Here...", "")
                        .append_item(M_EXPORT_CAT, "Export Category to OPML...", "");
                }
                "feed" => {
                    builder = builder
                        .append_item(M_REFRESH_FEED, "Refresh Feed", "")
                        .append_item(M_MARKALL_CTX, "Mark All Items as Read", "")
                        .append_item(M_EDIT_FEED, "Edit Feed...", "")
                        .append_item(M_RESET_TITLE, "Reset Title to Feed Default", "")
                        .append_item(M_COPY_FEED_URL, "Copy Feed URL", "")
                        .append_check_item(M_FEED_NOTIF, "Notifications for This Feed", "")
                        .append_item(M_REMOVE_FEED, "Remove Feed", "")
                        .append_separator()
                        .append_item(M_FEED_PROPS, "Feed Properties...", "")
                        .append_item(M_SET_CAT, "Set Feed Category...", "")
                        .append_item(M_ARCHIVE, "Rescan Archive", "");
                }
                "smart_root" => {
                    builder = builder.append_item(M_NEW_SF, "New Smart Folder...", "");
                }
                "smart" => {
                    builder = builder
                        .append_item(M_EDIT_SF, "Edit Smart Folder...", "")
                        .append_item(M_DEL_SF, "Delete Smart Folder", "")
                        .append_item(M_NEW_SF, "New Smart Folder...", "");
                }
                _ => {
                    builder = builder
                        .append_item(M_REFRESH, "Refresh Feeds", "")
                        .append_item(M_STOP_REFRESH, "Stop Refresh", "")
                        .append_item(M_SYNC, "Sync Providers", "")
                        .append_separator()
                        .append_item(M_ADD_CAT, "Add Category", "")
                        .append_item(M_NEW_SF, "New Smart Folder...", "");
                }
            }
            let mut menu = builder.build();
            // Reflect the selection: the Notifications check
            // (checked = notifications ON for the selected feed)
            // and the Image Alt Text radio state.
            let (feed_id, db) = {
                let st = state_ctx_tree.borrow();
                (st.selected_feed.clone(), st.db.is_some())
            };
            if kind == "feed"
                && let Some(fid) = &feed_id
            {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let excluded = notification_excluded_ids(&cfg).contains(fid);
                menu.check_item(M_FEED_NOTIF, !excluded);
                let mode = if db {
                    state_ctx_tree
                        .borrow()
                        .db
                        .as_ref()
                        .and_then(|d| d.feed_show_images(fid).ok())
                        .flatten()
                } else {
                    None
                };
                let sub = Menu::builder()
                    .append_radio_item(M_IMGALT_DEFAULT, "Use &Default Setting", "")
                    .append_radio_item(M_IMGALT_ALWAYS, "&Always Show Image Alt Text", "")
                    .append_radio_item(M_IMGALT_NEVER, "&Never Show Image Alt Text", "")
                    .build();
                sub.check_item(
                    match mode {
                        None => M_IMGALT_DEFAULT,
                        Some(true) => M_IMGALT_ALWAYS,
                        Some(false) => M_IMGALT_NEVER,
                    },
                    true,
                );
                menu.append_submenu(sub, "Image Alt Text", "");
            }
            tree.popup_menu(&mut menu, None);
        });
        let state_ctx_list = state.clone();
        let chapter_urls_ctx = chapter_link_urls.clone();
        list.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            // Article list context menu, aligned to Python's
            // on_list_context_menu (Batch 41a / Audit 6 G1.2):
            // selection-aware count labels, conditional media /
            // image / chapter items. Rust-only actions Python
            // keeps in its (removed) Article menu stay after a
            // separator at the end.
            let (count, article, chapter_hrefs, has_history) = {
                let st = state_ctx_list.borrow();
                let indices = selected_article_indices(&list, &st);
                let article = st
                    .selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned();
                let mut hrefs: Vec<(String, String)> = Vec::new();
                let mut has_history = false;
                if let (Some(a), Some(db)) = (&article, &st.db) {
                    has_history = db.count_article_versions(&a.id).unwrap_or(0) > 0;
                    if let Ok(chapters) = db.chapters(&a.id) {
                        for ch in chapters {
                            let Some(href) = ch.href.as_deref().filter(|h| !h.trim().is_empty())
                            else {
                                continue;
                            };
                            let resolved =
                                if href.starts_with("http://") || href.starts_with("https://") {
                                    href.to_string()
                                } else if let Some(rest) = href.strip_prefix('/') {
                                    match a.url.split_once("://") {
                                        Some((scheme, after)) => format!(
                                            "{scheme}://{}/{}",
                                            after.split('/').next().unwrap_or(""),
                                            rest
                                        ),
                                        None => continue,
                                    }
                                } else {
                                    match a.url.rsplit_once('/') {
                                        Some((base, _)) => format!("{base}/{href}"),
                                        None => continue,
                                    }
                                };
                            let label = ch
                                .title
                                .as_deref()
                                .filter(|t| !t.trim().is_empty())
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| resolved.clone());
                            hrefs.push((label, resolved));
                            if hrefs.len() >= M_CHAPTER_LINK_SLOTS as usize {
                                break;
                            }
                        }
                    }
                }
                (indices.len(), article, hrefs, has_history)
            };
            *chapter_urls_ctx.borrow_mut() = chapter_hrefs.iter().map(|(_, u)| u.clone()).collect();
            let multi = count > 1;
            let has_media = article
                .as_ref()
                .is_some_and(|a| a.media_url.as_deref().is_some_and(|u| !u.is_empty()));
            let direct_media = article.as_ref().is_some_and(has_direct_media_link);
            let has_image = article
                .as_ref()
                .is_some_and(|a| first_image_url(a.content.as_deref()).is_some());
            let in_queue = article.as_ref().is_some_and(|a| {
                dialogs::load_queue(&config_path())
                    .iter()
                    .any(|e| e.get("article_id").and_then(|v| v.as_str()) == Some(a.id.as_str()))
            });
            let mut builder = Menu::builder()
                .append_item(M_ACTIVATE, "Open Article", "")
                .append_item(M_OPEN_BROWSER, "Open in Default Browser", "")
                .append_separator();
            if multi {
                builder = builder
                    .append_item(M_MARK_READ_SEL, &format!("Mark {count} as &Read"), "")
                    .append_item(M_MARK_UNREAD_SEL, &format!("Mark {count} as &Unread"), "")
                    .append_item(
                        M_DELETE_ARTICLE,
                        &format!("Delete {count} Articles\tDel"),
                        "",
                    );
            } else {
                builder = builder
                    .append_item(M_MARK_READ_SEL, "Mark as &Read", "")
                    .append_item(M_MARK_UNREAD_SEL, "Mark as &Unread", "")
                    .append_item(M_DELETE_ARTICLE, "Delete Article\tDel", "");
            }
            builder = builder.append_separator().append_item(
                M_COPY_URL,
                if multi { "Copy Links" } else { "Copy Link" },
                "",
            );
            if article.is_some() {
                builder = builder.append_item(
                    M_COPY_TEXT,
                    &if multi {
                        format!("Copy Text ({count} articles)")
                    } else {
                        "Copy Text".to_string()
                    },
                    "",
                );
                builder = builder.append_item(M_VIEW_DESC, "View Feed Description...", "");
                if direct_media {
                    builder = builder.append_item(M_COPY_MEDIA, "Copy Media Link", "");
                }
                if has_media {
                    builder = builder.append_item(M_DOWNLOAD, "Download", "").append_item(
                        M_DOWNLOAD_AS,
                        "Download As...",
                        "",
                    );
                } else {
                    builder = builder.append_item(M_DETECT_AUDIO, "Detect Audio", "");
                }
                if has_image {
                    builder = builder.append_item(M_COPY_IMAGE, "Copy Image Link", "");
                }
                builder = builder.append_separator();
                if multi {
                    builder = builder
                        .append_item(M_QUEUE_ADD_SEL, &format!("Add {count} to Play Queue"), "")
                        .append_item(
                            M_QUEUE_REMOVE_SEL,
                            &format!("Remove {count} from Play Queue"),
                            "",
                        );
                } else if in_queue {
                    builder = builder.append_item(M_QUEUE_REMOVE_SEL, "Remove from Play Queue", "");
                } else {
                    builder = builder.append_item(M_QUEUE_ADD_SEL, "Add to Play Queue", "");
                }
                builder = builder.append_item(M_QUEUE_OPEN, "Open Play Queue...", "");
            }
            let mut menu = builder.build();
            if !chapter_hrefs.is_empty() {
                let mut sub = Menu::builder();
                for (slot, (label, _url)) in chapter_hrefs.iter().enumerate() {
                    sub = sub.append_item(
                        M_CHAPTER_LINK_BASE + slot as i32,
                        &format!("Open {label}"),
                        "",
                    );
                }
                menu.append_submenu(sub.build(), "Chapter Links", "");
            }
            if article.is_some() {
                menu.append(M_FAVORITE, "Toggle &Favorite", "", ItemKind::Normal);
                if has_history {
                    menu.append(M_HISTORY, "View &History...", "", ItemKind::Normal);
                }
                menu.append_separator();
                menu.append(M_FULLTEXT, "Read &Full Text", "", ItemKind::Normal);
                menu.append(M_PLAY, "&Play Media", "", ItemKind::Normal);
                menu.append(M_CAST, "&Cast...", "", ItemKind::Normal);
                menu.append(M_STOP_CAST, "Stop &AirPlay Cast", "", ItemKind::Normal);
                menu.append(M_TRANSLATE, "&Translate Article", "", ItemKind::Normal);
                menu.append(
                    M_ARTICLE_WINDOW,
                    "Open in Article &Window",
                    "",
                    ItemKind::Normal,
                );
                menu.append(M_DELETED, "De&leted Articles...", "", ItemKind::Normal);
            } else {
                // Python keeps the article actions visible but
                // disabled with no target article.
                for id in [
                    M_ACTIVATE,
                    M_OPEN_BROWSER,
                    M_MARK_READ_SEL,
                    M_MARK_UNREAD_SEL,
                    M_COPY_URL,
                ] {
                    menu.enable_item(id, false);
                }
            }
            list.popup_menu(&mut menu, None);
        });
        // Reader content context menu (Python's
        // _show_content_context_menu): Copy / Select All only.
        // The stock ids fall through the frame menu handler to
        // wx's default text-control handling, like the Edit
        // menu's items.
        reader.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            let mut menu = Menu::builder()
                .append_item(5032, "&Copy\tCtrl+C", "")
                .append_separator()
                .append_item(5037, "Select &All\tCtrl+A", "")
                .build();
            reader.popup_menu(&mut menu, None);
        });

        // Reader focus starts the full-text load at once
        // (Audit 8 G02; Python on_content_focus with
        // force=True). The load itself runs through the Read
        // Full Text action via the drain timer, so cache,
        // request guards and translation apply unchanged.
        {
            let state_focus = state.clone();
            reader.bind_internal(EventType::SET_FOCUS, move |ev| {
                ev.skip(true);
                let mut st = state_focus.borrow_mut();
                if st.selected_article.is_some() {
                    schedule_auto_fulltext(&mut st, true);
                }
            });
        }

        // Enter (item activation): play when the article has
        // media, otherwise open its link in the browser
        // (Brandon's rule, 2026-10-30).
        let activate_mouse = act_activate.clone();
        list.bind_internal(EventType::LIST_ITEM_ACTIVATED, move |ev| {
            ev.skip(false);
            activate_mouse();
        });

        // Keyboard dispatch (PARITY 7.5): a frame-level char hook
        // resolves keys through the shortcut registry (defaults +
        // user overrides), exactly Python's model. Handled keys are
        // consumed; everything else skips through to the control.
        {
            let state_hook = state.clone();
            let keymap_hook = keymap.clone();
            let h_toggle_read = act_toggle_read.clone();
            let h_activate = act_activate.clone();
            let h_play = act_play.clone();
            let h_find = act_find.clone();
            let h_find_next = act_find_next.clone();
            let f1_frame = frame;
            let f1_language = language.clone();
            let f1_tree = tree.get_handle() as usize;
            let f1_list = list.get_handle() as usize;
            let f1_reader = reader.get_handle() as usize;
            let f1_search = search_box.get_handle() as usize;
            frame.bind_internal(EventType::CHAR_HOOK, move |ev| {
                let code = ev.get_key_code().unwrap_or(0);
                let ctrl = ev.control_down() || ev.cmd_down();
                let shift = ev.shift_down();
                if ev.alt_down() {
                    ev.skip(true);
                    return;
                }
                // F1: context help for the control that has focus
                // (Python's help_topics resolution, main-window
                // subset: the registry's dialog/menu/command maps
                // ride along in core for the dialogs' own F1 keys).
                if code == 340 && !ctrl && !shift {
                    let focused = ev
                        .get_event_object()
                        .map(|w| w.get_handle() as usize)
                        .unwrap_or(0);
                    let topic = if focused == f1_tree {
                        "feed-tree"
                    } else if focused == f1_list {
                        "article-list"
                    } else if focused == f1_reader {
                        "reading-pane"
                    } else if focused == f1_search {
                        "search-field"
                    } else {
                        "main-window"
                    };
                    help::show_help_topic(&f1_frame, &f1_language, Some(topic));
                    ev.skip(false);
                    return;
                }
                // Enter in the article list activates the
                // article (play media / open in browser — the
                // on_item_activated binding covers the mouse
                // double-click; this covers the keyboard, and
                // vetoing the key keeps wx from also firing
                // ITEM_ACTIVATED for it).
                if code == 13 && !ctrl && !shift {
                    let cfg =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    if reader.has_focus()
                        && cfg.get_bool("article_structure_links", false)
                        && open_reader_link(&reader, &frame)
                    {
                        ev.skip(false);
                        return;
                    }
                    let in_list = {
                        let st = state_hook.borrow();
                        st.focus_list && !st.search_focused
                    };
                    if in_list {
                        h_activate();
                        ev.skip(false);
                    } else {
                        ev.skip(true);
                    }
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
                    Some("view.focus_search") => {
                        search_box.set_focus();
                        true
                    }
                    Some("feeds.edit_selected") => {
                        let category = state_hook
                            .borrow()
                            .selected_feed
                            .as_deref()
                            .is_some_and(|id| id.starts_with("cat:"));
                        frame.process_menu_command(if category {
                            M_RENAME_CAT
                        } else {
                            M_EDIT_FEED
                        })
                    }
                    Some("tools.announce_version") => {
                        announce(
                            "general",
                            &format!("BlindRSS {}", env!("CARGO_PKG_VERSION")),
                        );
                        true
                    }
                    Some(command)
                        if command.starts_with("player.") || command.starts_with("speed.") =>
                    {
                        if player::dispatch_command(command) {
                            true
                        } else if command == "player.play_pause" {
                            h_play();
                            true
                        } else {
                            false
                        }
                    }
                    Some(command) => {
                        shortcut_menu_id(command).is_some_and(|id| frame.process_menu_command(id))
                    }
                    None => false,
                };
                ev.skip(!handled);
            });
        }

        // Background watcher (Python's 45-second cookie-import
        // tick): first pass at startup, then every 45 seconds,
        // never overlapping itself.
        let watcher_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = watcher_in_flight.clone();
            let tx0 = tx.clone();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                run_watcher_once(&tx0);
                flag.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let watch_timer = Box::leak(Box::new(Timer::new(&frame)));
        let tx_watch = tx.clone();
        watch_timer.on_tick(move |_| {
            if watcher_in_flight.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let flag = watcher_in_flight.clone();
            let tx2 = tx_watch.clone();
            std::thread::spawn(move || {
                run_watcher_once(&tx2);
                flag.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        });
        watch_timer.start(45_000, false);

        let timer = Box::leak(Box::new(Timer::new(&frame)));
        let tray_menu_tick = tray_menu.clone();
        let tray_unread_last = Rc::new(std::cell::Cell::new(tray_unread0));
        let state_tick = state.clone();
        let frame_tick = frame;
        let rich_view_tick = rich_view.clone();
        let rich_loading_tick = rich_loading.clone();
        let rx_tick = rx.clone();
        let tx_tick = tx.clone();
        let auto_ft_tick = act_fulltext.clone();
        timer.on_tick(move |_| {
            if frame_tick.is_shown() && frame_tick.is_iconized() {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if cfg.get_bool("minimize_to_tray", true) {
                    frame_tick.show(false);
                }
            }
            // Coalesced tree refill (see the tree selection
            // handler): fill once for the landed-on node.
            {
                let mut st = state_tick.borrow_mut();
                if st.refill_pending {
                    st.refill_pending = false;
                    refill_articles(&mut st, &list);
                }
            }
            // Automatic full-text load (Audit 8 G02): a
            // schedule set by selection or reader focus fires
            // once when due, through the Read Full Text
            // action so every guard in it applies. A schedule
            // whose generation moved on is cancelled silently.
            {
                let fire = {
                    let mut st = state_tick.borrow_mut();
                    match st.auto_load {
                        Some((generation, due)) if generation == st.reader_generation => {
                            if std::time::Instant::now() >= due {
                                st.auto_load = None;
                                true
                            } else {
                                false
                            }
                        }
                        Some(_) => {
                            st.auto_load = None;
                            false
                        }
                        None => false,
                    }
                };
                if fire {
                    auto_ft_tick();
                }
            }
            // Tray "Unread: {count}" label (Batch 41a): refresh
            // when the total changes.
            {
                let total = state_tick
                    .borrow()
                    .db
                    .as_ref()
                    .and_then(|db| db.unread_count(None).ok())
                    .unwrap_or(0);
                if tray_unread_last.get() != total {
                    tray_unread_last.set(total);
                    if let Some(item) = tray_menu_tick.borrow().find_item(TRAY_UNREAD) {
                        item.set_label(&format!("Unread: {total}"));
                    }
                }
            }
            let mut new_total = 0usize;
            let mut finished = false;
            loop {
                let msg = rx_tick.borrow().try_recv();
                match msg {
                    Ok(RefreshMsg::FeedDone(path, feed, Ok(outcome))) => {
                        let mut st = state_tick.borrow_mut();
                        if path != st.store_path {
                            continue;
                        }
                        // The worker already stored through its
                        // own connection (Audit 8 D22); the count
                        // that reaches the status line and
                        // notifications is Python's new_items:
                        // rule-removed and notification-suppressed
                        // articles do not count (Audit 8 D13).
                        new_total += outcome.notify_count;
                        if outcome.notify_count > 0 {
                            st.refresh_new.push((
                                feed.id.clone(),
                                feed.title.clone(),
                                outcome.notify_count,
                            ));
                        }
                    }
                    Ok(RefreshMsg::FeedDone(path, feed, Err(e))) => {
                        let mut st = state_tick.borrow_mut();
                        if path != st.store_path {
                            continue;
                        }
                        st.refresh_failed = true;
                        frame_tick.set_status_text(&format!("{}: {e}", feed.title), 0);
                    }
                    Ok(RefreshMsg::Added(_url, Ok(feed), _add_category)) => {
                        // The subscribe worker resolved, inserted
                        // and stored the feed (Audit 8 F01/D22);
                        // the drain only takes the new row in.
                        let mut st = state_tick.borrow_mut();
                        let title = feed.title.clone();
                        st.feeds.push(feed);
                        rebuild_tree(&mut st, &tree);
                        refill_articles(&mut st, &list);
                        frame_tick.set_status_text(&format!("Added {title}"), 0);
                    }
                    Ok(RefreshMsg::Added(url, Err(e), _)) => {
                        frame_tick.set_status_text(&format!("Could not add {url}: {e}"), 0);
                    }
                    Ok(RefreshMsg::ActivateWindow) => {
                        frame_tick.show(true);
                        frame_tick.raise();
                    }
                    Ok(RefreshMsg::StatusNote(note)) => {
                        frame_tick.set_status_text(&note, 0);
                    }
                    Ok(RefreshMsg::ToggleWindow) => {
                        // Global show/hide hotkey (Windows): hide
                        // to the tray when this window is in front,
                        // otherwise bring it forward and take the
                        // foreground the hotkey press grants.
                        #[cfg(target_os = "windows")]
                        {
                            let hwnd = frame_tick.get_handle() as isize;
                            if frame_tick.is_shown()
                                && !frame_tick.is_iconized()
                                && hotkey::foreground_is(hwnd)
                            {
                                frame_tick.show(false);
                            } else {
                                if frame_tick.is_iconized() {
                                    frame_tick.iconize(false);
                                }
                                frame_tick.show(true);
                                frame_tick.raise();
                                hotkey::set_foreground(hwnd);
                            }
                        }
                    }
                    Ok(RefreshMsg::DepsMissing(missing)) => {
                        // Missing tools resolve automatically —
                        // no prompt (Brandon's order): yt-dlp via
                        // the managed download, ffmpeg/ffprobe via
                        // dependency::install_ffmpeg. Progress and
                        // outcomes land in the status bar through
                        // YtdlpUpdated / FfmpegInstalled.
                        frame_tick.set_status_text(
                            &format!("Installing missing tools: {}...", missing.join(", ")),
                            0,
                        );
                        if missing.iter().any(|n| n == "yt-dlp") {
                            let tx = tx_tick.clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(RefreshMsg::YtdlpUpdated(
                                    blindrss_core::dependency::update_ytdlp(&data_dir()),
                                ));
                            });
                        }
                        if missing.iter().any(|n| n == "ffmpeg" || n == "ffprobe") {
                            let tx = tx_tick.clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(RefreshMsg::FfmpegInstalled(
                                    blindrss_core::dependency::install_ffmpeg(&data_dir()),
                                ));
                            });
                        }
                    }
                    Ok(RefreshMsg::FfmpegInstalled(result)) => match result {
                        Ok(bin_dir) => {
                            // SAFETY: UI thread, same trade-off
                            // as apply_http_config's env writes.
                            // PATH first so the finders below (and
                            // yt-dlp later) see the managed pair.
                            unsafe {
                                let sep = if cfg!(windows) { ";" } else { ":" };
                                let cur = std::env::var("PATH").unwrap_or_default();
                                let bd = bin_dir.to_string_lossy().to_string();
                                if !cur.split(sep).any(|d| d == bd) {
                                    std::env::set_var("PATH", format!("{bd}{sep}{cur}"));
                                }
                                if let Some(f) = blindrss_core::silence::find_ffmpeg() {
                                    std::env::set_var("BLINDRSS_FFMPEG", &f);
                                }
                                if let Some(p) = blindrss_core::dependency::find_ffprobe() {
                                    std::env::set_var("BLINDRSS_FFPROBE", &p);
                                }
                            }
                            frame_tick.set_status_text(
                                &format!("ffmpeg and ffprobe installed: {}", bin_dir.display()),
                                0,
                            );
                            announce("general", "ffmpeg installed");
                        }
                        Err(e) => frame_tick.set_status_text(&format!("ffmpeg install: {e}"), 0),
                    },
                    Ok(RefreshMsg::YtdlpUpdated(result)) => match result {
                        Ok(path) => {
                            // SAFETY: UI thread, same trade-off
                            // as apply_http_config's env writes.
                            unsafe {
                                std::env::set_var("BLINDRSS_YTDLP", &path);
                            }
                            frame_tick.set_status_text(
                                &format!("yt-dlp installed: {}", path.display()),
                                0,
                            );
                            announce("general", "yt-dlp installed");
                        }
                        Err(e) => frame_tick.set_status_text(&format!("yt-dlp download: {e}"), 0),
                    },
                    Ok(RefreshMsg::Translated(request, result)) => {
                        // Audit 8 O08: a late translation applies
                        // only to the article, generation and
                        // variant it was requested under. A
                        // delayed A result after selecting B —
                        // or after an account, provider,
                        // language or model change — is dropped,
                        // never written over the current article.
                        let cfg =
                            blindrss_core::config::Config::load(&config_path())
                                .unwrap_or_default();
                        let current_variant = translation_variant(&cfg);
                        let st = state_tick.borrow();
                        let selected = st
                            .selected_article
                            .and_then(|i| st.articles.get(i))
                            .cloned();
                        let current = selected.as_ref().is_some_and(|a| {
                            request.is_current(st.reader_generation, &a.id, &current_variant)
                        });
                        if current && let Some(article) = selected {
                            match result {
                                Ok(text) => {
                                    reader.set_value(&format!("{}\n\n{}", article.title, text));
                                    frame_tick.set_status_text("Article translated", 0);
                                    // The rich document is an
                                    // output too: refresh it so
                                    // toggling modes stays
                                    // truthful.
                                    if cfg.get_bool("full_text_rich_view", false)
                                        && rich_view_tick.as_ref().is_some()
                                    {
                                        spawn_rich_fetch(&tx_tick, &article);
                                    }
                                }
                                Err(e) => {
                                    frame_tick.set_status_text(&format!("Translate: {e}"), 0)
                                }
                            }
                        }
                    }
                    Ok(RefreshMsg::ProviderAction(result)) => {
                        let mut st = state_tick.borrow_mut();
                        st.refreshing = false;
                        st.last_refresh_end = Some(std::time::Instant::now());
                        st.feeds = st
                            .db
                            .as_ref()
                            .and_then(|db| db.feeds().ok())
                            .unwrap_or_default();
                        rebuild_tree(&mut st, &tree);
                        refill_articles(&mut st, &list);
                        match result {
                            Ok(msg) => {
                                frame_tick.set_status_text(&msg, 0);
                                announce("general", &msg);
                                drop(st);
                                refresh_side_effects(&tray, &[], false);
                            }
                            Err(e) => frame_tick.set_status_text(&format!("Provider: {e}"), 0),
                        }
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
                    Ok(RefreshMsg::FullText(request, outcome, first_html)) => {
                        let mut st = state_tick.borrow_mut();
                        if let blindrss_core::extract::ExtractOutcome::Article {
                            text,
                            source,
                            ..
                        } = &outcome
                        {
                            st.reader_cache.insert(
                                request.key.clone(),
                                text.clone(),
                                source,
                                request.generation,
                            );
                        }
                        if st
                            .pending_fulltext
                            .as_ref()
                            .is_some_and(|(generation, key)| {
                                *generation == request.generation && key == &request.key
                            })
                        {
                            st.pending_fulltext = None;
                        }
                        let article = st
                            .articles
                            .iter()
                            .find(|a| a.id == request.article_id)
                            .cloned();
                        // Metadata enrichment: the fetched page
                        // declares author/tags the feed omitted.
                        if let (Some(article), Some(html)) = (&article, &first_html)
                            && let Some(db) = st.db.as_ref()
                        {
                            let meta = blindrss_core::enrich::extract_metadata(html);
                            if !meta.author.is_empty() || !meta.tags.is_empty() {
                                let _ = db.apply_enrichment(&article.id, &meta);
                            }
                        }
                        let current = st.selected_article.and_then(|i| st.articles.get(i));
                        if !current.is_some_and(|a| {
                            request.is_current(st.reader_generation, &a.id, &a.url)
                        }) {
                            continue;
                        }
                        match (article, outcome) {
                            (
                                Some(article),
                                blindrss_core::extract::ExtractOutcome::Article {
                                    text,
                                    source,
                                    ..
                                },
                            ) => {
                                reader.set_value(&format!("{}\n\n{}", article.title, text));
                                frame_tick.set_status_text(&format!("Full text ({source})"), 0);
                            }
                            (_, blindrss_core::extract::ExtractOutcome::Gate { reason }) => {
                                frame_tick.set_status_text(
                                    &format!("Full text unavailable: {reason}"),
                                    0,
                                );
                            }
                            (_, blindrss_core::extract::ExtractOutcome::Empty) => {
                                frame_tick
                                    .set_status_text("Full text unavailable: nothing extracted", 0);
                            }
                            _ => {}
                        }
                    }
                    Ok(RefreshMsg::RichHtml(id, fragment)) => {
                        let st = state_tick.borrow();
                        let current = st
                            .selected_article
                            .and_then(|i| st.articles.get(i))
                            .map(|a| a.id.clone());
                        if current.as_deref() == Some(id.as_str())
                            && let (Some(wv), Some(article)) = (
                                rich_view_tick.as_ref(),
                                st.articles.iter().find(|a| a.id == id),
                            )
                        {
                            match fragment {
                                Some(frag) => {
                                    rich_loading_tick.set(true);
                                    wv.set_page(&rich_document(&frag), &article.url);
                                    frame_tick.set_status_text("Full text (rich view)", 0);
                                }
                                None => {
                                    frame_tick.set_status_text(
                                        "Rich full text unavailable; showing feed content",
                                        0,
                                    );
                                }
                            }
                        }
                    }
                    Ok(RefreshMsg::MediaReady(id, result)) => {
                        let mut st = state_tick.borrow_mut();
                        let article = st
                            .pending_play
                            .as_ref()
                            .filter(|a| a.id == id)
                            .cloned()
                            .or_else(|| st.articles.iter().find(|a| a.id == id).cloned());
                        let queue_ctx = st.pending_queue.take();
                        if article.is_some() {
                            st.pending_play = None;
                        }
                        match (article, result) {
                            (Some(article), Ok((stream, info, headers))) => {
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
                                match queue_ctx {
                                    Some((entries, index)) => player::open_player_queue(
                                        &frame_tick,
                                        article,
                                        stream,
                                        entries,
                                        index,
                                        headers,
                                    ),
                                    None => player::open_player(
                                        &frame_tick,
                                        article,
                                        stream,
                                        chapters,
                                        headers,
                                    ),
                                }
                            }
                            (_, Err(e)) => frame_tick.set_status_text(&format!("Play: {e}"), 0),
                            _ => {}
                        }
                    }
                    Ok(RefreshMsg::BackupDone(result)) => match result {
                        Ok(path) => {
                            // Python's completion message, including
                            // the sensitive-data notice: the backup
                            // holds passwords, API keys and cookies.
                            let md = MessageDialog::builder(
                                &frame_tick,
                                &format!(
                                    "Backup saved to {}\n\nIt contains your passwords, API keys and site cookies, so keep it private.",
                                    path.display()
                                ),
                                "Back Up Data",
                            )
                            .with_style(
                                MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                            )
                            .build();
                            md.show_modal();
                            md.destroy();
                        }
                        Err(e) => {
                            let md = MessageDialog::builder(
                                &frame_tick,
                                &format!("Backup failed: {e}"),
                                "Error",
                            )
                            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
                            .build();
                            md.show_modal();
                            md.destroy();
                        }
                    },
                    Ok(RefreshMsg::DownloadDone(article, result)) => match result {
                        Ok(path) => {
                            let indexed = blindrss_core::config::Config::load(&config_path())
                                .map_err(|e| e.to_string())
                                .and_then(|mut cfg| {
                                    blindrss_core::download::record_article_download(
                                        &mut cfg, &article, &path,
                                    )?;
                                    cfg.save(&config_path()).map_err(|e| e.to_string())
                                });
                            match indexed {
                                Ok(()) => frame_tick
                                    .set_status_text(&format!("Downloaded {}", path.display()), 0),
                                Err(e) => frame_tick.set_status_text(
                                    &format!(
                                        "Downloaded {}; could not remember file: {e}",
                                        path.display()
                                    ),
                                    0,
                                ),
                            }
                        }
                        Err(e) => frame_tick.set_status_text(&format!("Download: {e}"), 0),
                    },
                    Ok(RefreshMsg::AudioDetected(article_id, found)) => {
                        let mut st = state_tick.borrow_mut();
                        match found {
                            Some((media_url, media_type)) => {
                                if let Some(db) = st.db.as_ref() {
                                    let _ = db.update_article_media(
                                        &article_id,
                                        &media_url,
                                        media_type.as_deref(),
                                    );
                                }
                                if let Some(row) =
                                    st.articles.iter_mut().find(|a| a.id == article_id)
                                {
                                    row.media_url = Some(media_url);
                                    row.media_type = media_type;
                                }
                                refill_articles(&mut st, &list);
                                frame_tick.set_status_text("Audio detected and added", 0);
                                announce("general", "Audio detected and added");
                            }
                            None => {
                                frame_tick.set_status_text("Detect Audio: no audio found", 0);
                                announce("general", "No audio found");
                            }
                        }
                    }
                    Ok(RefreshMsg::YoutubeText(request, result)) => {
                        let mut st = state_tick.borrow_mut();
                        if let Ok(text) = &result {
                            st.reader_cache.insert(
                                request.key.clone(),
                                text.clone(),
                                "youtube",
                                request.generation,
                            );
                        }
                        if st
                            .pending_fulltext
                            .as_ref()
                            .is_some_and(|(generation, key)| {
                                *generation == request.generation && key == &request.key
                            })
                        {
                            st.pending_fulltext = None;
                        }
                        let Some(article) = st.selected_article.and_then(|i| st.articles.get(i))
                        else {
                            continue;
                        };
                        if !request.is_current(st.reader_generation, &article.id, &article.url) {
                            continue;
                        }
                        let title = article.title.clone();
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
                    Ok(RefreshMsg::CastDevices(devices, article, url)) => {
                        let device = if devices.len() == 1 {
                            devices.into_iter().next()
                        } else {
                            dialogs::show_cast_picker(&frame_tick, &devices)
                        };
                        restore_focus(&state_tick.borrow(), &tree, &list);
                        if let Some(device) = device {
                            // Every device kind goes through the
                            // persistent controls window now: its
                            // session layer dispatches Chromecast,
                            // AirPlay/RAOP and the unified UPnP
                            // protocols, and carries the play
                            // queue (casting-depth integration).
                            let mime = article
                                .media_type
                                .clone()
                                .filter(|t| t.contains('/'))
                                .unwrap_or_else(|| "audio/mpeg".into());
                            let reachable =
                                if url.starts_with("http://") || url.starts_with("https://") {
                                    blindrss_core::proxy::stream_url(&url)
                                } else {
                                    blindrss_core::proxy::file_url(std::path::Path::new(&url))
                                };
                            casting::open(device, article.title, reachable, mime);
                        }
                    }
                    Ok(RefreshMsg::CastDone(result)) => match result {
                        Ok(msg) => frame_tick.set_status_text(&msg, 0),
                        Err(e) => frame_tick.set_status_text(&format!("Cast: {e}"), 0),
                    },
                    Ok(RefreshMsg::AllDone(path)) => {
                        if path != state_tick.borrow().store_path {
                            continue;
                        }
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
                let new_by_feed = std::mem::take(&mut st.refresh_new);
                let failed = std::mem::take(&mut st.refresh_failed);
                let store_path = st.store_path.clone();
                let refreshed_feeds = st.feeds.clone();
                drop(st);
                refresh_side_effects(&tray, &new_by_feed, failed);
                // Podcast archive (gap 6): every refresh has
                // committed, so queue the due feeds' archive
                // scans — Python claims and queues only after
                // the live-feed transaction commits, so a fast
                // archive job can never race a save. The claim
                // inside queue_local_scan enforces the due
                // rules; off the UI thread because claiming is
                // a database write per feed.
                {
                    let cfg =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    let settings = blindrss_core::archive_store::ArchiveSettings::from_config(&cfg);
                    if settings.enabled {
                        std::thread::spawn(move || {
                            for feed in &refreshed_feeds {
                                blindrss_core::archive_store::queue_local_scan(
                                    &store_path,
                                    feed,
                                    &settings,
                                    None,
                                );
                            }
                        });
                    }
                }
            }
        });
        timer.start(150, false);

        let sched_timer = Box::leak(Box::new(Timer::new(&frame)));
        let state_sched = state.clone();
        let refresh_sched = act_refresh.clone();
        let force_sched = refresh_force.clone();
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
                if st
                    .refresh_paused_until
                    .is_some_and(|until| std::time::Instant::now() < until)
                {
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
                let workload = cfg.get_str(
                    "automatic_feed_refresh_workload",
                    if cfg.get_bool("ignore_feed_cache", false) {
                        "always_full"
                    } else {
                        "startup_full"
                    },
                );
                force_sched.set(workload == "always_full");
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
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            frame.show(!cfg.get_bool("start_in_system_tray", false));
        }
        // A copy relaunched by a ship-shape update comes to the
        // front (no-op on an ordinary start).
        ship_shape::ui::bring_to_front_after_update(&frame);
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
                let force_start = refresh_force.clone();
                startup_timer.on_tick(move |_| {
                    let cfg =
                        blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                    force_start.set(
                        cfg.get_str("automatic_feed_refresh_workload", "startup_full") != "cached",
                    );
                    refresh_start();
                });
                startup_timer.start(1200, true);
            }
            // Startup update check (auto_check_updates, default
            // on): ship-shape with the Automatic trigger — only an
            // available update surfaces, as a dialog the user
            // confirms before anything downloads.
            if cfg.get_bool("auto_check_updates", true) {
                let update_timer = Box::leak(Box::new(Timer::new(&frame)));
                let update_start = act_update_auto.clone();
                update_timer.on_tick(move |_| {
                    update_start();
                });
                update_timer.start(4000, true);
            }
        }
    });
}

#[cfg(test)]
mod ui_parity_tests {
    use super::*;

    fn article(url: &str, media: Option<&str>) -> Article {
        Article {
            id: "episode".into(),
            feed_id: "feed".into(),
            title: "Episode".into(),
            url: url.into(),
            media_url: media.map(str::to_owned),
            media_type: Some("audio/mpeg".into()),
            content: None,
            description: None,
            date: None,
            author: None,
            is_read: false,
            is_favorite: false,
        }
    }

    #[test]
    fn auto_fulltext_triggers_match_python_selection_rules() {
        // Audit 8 G02/O07: translation on, macOS, a forum
        // thread or a YouTube video each start the automatic
        // load on selection; a plain article on another
        // platform waits for reader focus.
        assert!(auto_fulltext_on_select(true, false, false, false));
        assert!(auto_fulltext_on_select(false, true, false, false));
        assert!(auto_fulltext_on_select(false, false, true, false));
        assert!(auto_fulltext_on_select(false, false, false, true));
        assert!(!auto_fulltext_on_select(false, false, false, false));
    }

    #[test]
    fn reader_cache_keys_separate_variants_and_carry_the_article_prefix() {
        // Audit 8 O07/G16: the translation variant is part of
        // the key (translated and original renderings never
        // collide), and every key for one article shares its
        // `id:` prefix so an image-alt change can invalidate
        // exactly that article's entries.
        let a = article("https://example.com/post", None);
        let plain = reader_cache_key(&a, false, "");
        let translated = reader_cache_key(&a, false, "::tr[grok:en]");
        let alt = reader_cache_key(&a, true, "");
        assert_ne!(plain, translated);
        assert_ne!(plain, alt);
        for key in [&plain, &translated, &alt] {
            assert!(
                key.starts_with("episode:"),
                "key {key} lacks the article prefix"
            );
        }
        let other = {
            let mut b = article("https://example.com/post", None);
            b.id = "other".into();
            reader_cache_key(&b, false, "")
        };
        assert!(!other.starts_with("episode:"));
    }

    #[test]
    fn copy_episode_link_prefers_playable_enclosure_over_homepage() {
        let a = article(
            "https://example.com/?source=podcast",
            Some("https://cdn.example/episode.mp3"),
        );
        assert_eq!(article_copy_url(&a), "https://cdn.example/episode.mp3");
        let a = article(
            "https://example.com/episode",
            Some("https://cdn.example/episode.mp3"),
        );
        assert_eq!(article_copy_url(&a), "https://example.com/episode");
    }

    #[test]
    fn youtube_watch_page_is_not_a_direct_copyable_media_link() {
        let a = article(
            "https://youtube.com/watch?v=abc",
            Some("https://youtube.com/watch?v=abc"),
        );
        assert!(!has_direct_media_link(&a));
        assert_eq!(article_copy_url(&a), a.url);
    }

    #[test]
    fn python_remembered_views_restore_through_app_routing() {
        // Audit 8 G20: Python-written category + filter prefix.
        let (selected, filter) = restored_view_state("unread:category:News / Tech");
        assert_eq!(selected.as_deref(), Some("cat:News / Tech"));
        assert_eq!(filter, 1);
        let (selected, filter) = restored_view_state("read:feed-42");
        assert_eq!(selected.as_deref(), Some("feed-42"));
        assert_eq!(filter, 2);
        // Deleted has no Rust tree node (it is a dialog): All
        // view, and the stored value is not rewritten by
        // restoring. Favorites DOES have a node (Audit 8 G06)
        // and restores onto it.
        let (selected, filter) = restored_view_state("deleted:all");
        assert_eq!(selected, None);
        assert_eq!(filter, 0);
        let (selected, _) = restored_view_state("favorites:all");
        assert_eq!(selected.as_deref(), Some("favorites:all"));
        // Saving a Rust selection writes the Python format.
        assert_eq!(
            remembered_view_value(Some("cat:News / Tech"), 1),
            "unread:category:News / Tech"
        );
        assert_eq!(remembered_view_value(None, 0), "all");
        assert_eq!(remembered_view_value(Some("smart:7"), 1), "smart:7");
    }

    #[test]
    fn youtube_recommendation_view_scopes() {
        // Audit 8 Task 10 (D19/D20): which views include the
        // account's recommendations, and which Mark All Read
        // scopes persist to their table.
        let mut cfg = blindrss_core::config::Config::default();
        cfg.set_path(
            "youtube_account_refresh_token",
            serde_json::json!("refresh-token-task10"),
        );
        cfg.set_path(
            "youtube_account_category",
            serde_json::json!("Media / YouTube"),
        );
        // The account category and its ancestors include them;
        // siblings and unrelated categories do not.
        assert!(category_includes_recommendations(&cfg, "Media / YouTube"));
        assert!(category_includes_recommendations(&cfg, "Media"));
        assert!(!category_includes_recommendations(&cfg, "Media / You"));
        assert!(!category_includes_recommendations(&cfg, "News"));
        // Mark scopes: All and the dedicated feed take the
        // current snapshot, Favorites takes favorites, a plain
        // feed takes none.
        let (account, fav) = recommendations_mark_scope(&cfg, None).unwrap();
        assert!(!account.is_empty() && !fav);
        let (_, fav) = recommendations_mark_scope(&cfg, Some("favorites:all")).unwrap();
        assert!(fav);
        let (_, fav) = recommendations_mark_scope(
            &cfg,
            Some(blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID),
        )
        .unwrap();
        assert!(!fav);
        let (_, fav) = recommendations_mark_scope(&cfg, Some("cat:Media")).unwrap();
        assert!(!fav);
        assert!(recommendations_mark_scope(&cfg, Some("cat:News")).is_none());
        assert!(recommendations_mark_scope(&cfg, Some("some-feed-id")).is_none());
        assert!(recommendations_mark_scope(&cfg, Some("smart:3")).is_none());
        // Signed out: no scope anywhere.
        let cfg = blindrss_core::config::Config::default();
        assert!(recommendations_mark_scope(&cfg, None).is_none());
    }

    #[test]
    fn mark_all_prompts_distinguish_global_and_node_scopes() {
        // Audit 8 G05: Python's main command asks globally,
        // tree context commands name their node (feed title
        // when known, category path, "this feed" fallback).
        assert_eq!(
            mark_all_prompt(None, None),
            "Mark all items in all feeds as read?"
        );
        assert_eq!(
            mark_all_prompt(Some("cat:News / Tech"), None),
            "Mark all items in category \"News / Tech\" as read?"
        );
        assert_eq!(
            mark_all_prompt(Some("feed-42"), Some("Daily Show")),
            "Mark all items in \"Daily Show\" as read?"
        );
        assert_eq!(
            mark_all_prompt(Some("feed-42"), None),
            "Mark all items in this feed as read?"
        );
        assert_eq!(
            mark_all_prompt(Some("feed-42"), Some("  ")),
            "Mark all items in this feed as read?"
        );
    }

    #[test]
    fn view_query_maps_tree_values_to_scopes() {
        // Audit 8 G03/G04: the tree's data values become core
        // scopes, and the filter/sort/search state rides along.
        fn base_state() -> State {
            State {
                db: None,
                store_path: PathBuf::new(),
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
                refresh_paused_until: None,
                read_filter: 1,
                media_filter: 2,
                sort_by: "name".to_string(),
                sort_ascending: true,
                view_total: -1,
                search_terms: vec!["rust".to_string()],
                search_title_only: true,
                ctx_mark: None,
                find_query: String::new(),
                search_focused: false,
                expanded_cats: std::collections::HashSet::new(),
                collapsed_cats: std::collections::HashSet::new(),
                cols_sig: String::new(),
                refill_pending: false,
                refresh_new: Vec::new(),
                refresh_failed: false,
                pending_play: None,
                pending_queue: None,
                reader_cache: blindrss_core::reader_cache::ReaderCache::default(),
                reader_generation: 0,
                pending_fulltext: None,
            }
        }
        use blindrss_core::db::ViewScope;
        let mut st = base_state();
        let q = view_query(&st);
        assert_eq!(q.scope, ViewScope::All);
        assert_eq!(q.read_filter, 1);
        assert_eq!(q.media_filter, 2);
        assert_eq!(q.sort_by, "name");
        assert!(q.ascending);
        assert_eq!(q.search_terms, ["rust"]);
        assert!(q.search_title_only);

        st.selected_feed = Some("cat:News / Tech".to_string());
        assert_eq!(
            view_query(&st).scope,
            ViewScope::Category("News / Tech".to_string())
        );
        st.selected_feed = Some("smart:7".to_string());
        assert_eq!(view_query(&st).scope, ViewScope::Smart("7".to_string()));
        st.selected_feed = Some("favorites:all".to_string());
        assert_eq!(view_query(&st).scope, ViewScope::Favorites);
        st.selected_feed = Some("feed-42".to_string());
        assert_eq!(
            view_query(&st).scope,
            ViewScope::Feed("feed-42".to_string())
        );
    }

    #[test]
    fn youtube_recommendations_merge_in_date_order() {
        // Audit 8 D19: merged views interleave recommendations
        // with RSS articles by stored date, newest first.
        let mk = |id: &str, date: &str| Article {
            id: id.into(),
            feed_id: "f".into(),
            title: id.into(),
            url: String::new(),
            content: None,
            description: None,
            date: Some(date.into()),
            author: None,
            is_read: false,
            is_favorite: false,
            media_url: None,
            media_type: None,
        };
        let base = vec![
            mk("new", "2026-11-03T10:00:00+00:00"),
            mk("old", "2026-11-01T10:00:00+00:00"),
        ];
        let extra = vec![mk("mid", "2026-11-02T10:00:00+00:00")];
        let merged = merge_recommendations(base, extra);
        let ids: Vec<&str> = merged.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["new", "mid", "old"]);
    }

    #[test]
    fn deleted_rows_leave_only_on_success() {
        // Audit 8 G07: a failed restore/purge keeps its row in
        // the Deleted Articles list; only succeeded rows leave,
        // order otherwise preserved. Identity is (feed, article).
        let mk = |feed: &str, id: &str| Article {
            id: id.into(),
            feed_id: feed.into(),
            title: format!("T{id}"),
            url: String::new(),
            content: None,
            description: None,
            date: None,
            author: None,
            is_read: false,
            is_favorite: false,
            media_url: None,
            media_type: None,
        };
        let rows = vec![mk("f1", "a"), mk("f2", "a"), mk("f1", "b")];
        let kept = retain_failed_deleted_rows(rows, &[("f2".to_string(), "a".to_string())]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].feed_id, "f1");
        assert_eq!(kept[0].id, "a");
        assert_eq!(kept[1].id, "b");
        // Display title falls back title -> url -> placeholder.
        let mut bare = mk("f1", "x");
        bare.title = String::new();
        assert_eq!(deleted_display_title(&bare), "(deleted article)");
        bare.url = "https://example.com/x".into();
        assert_eq!(deleted_display_title(&bare), "https://example.com/x");
    }

    #[test]
    fn registry_existing_actions_have_menu_routes() {
        for id in [
            "article.copy_text",
            "article.download_as",
            "article.toggle_read",
            "view.rich_view",
            "sort.description",
            "sort.status",
            "feeds.edit_selected",
            "tools.settings",
            "help.user_guide",
        ] {
            assert!(shortcut_menu_id(id).is_some(), "missing route: {id}");
        }
        assert_eq!(shortcut_menu_id("not.a.command"), None);
    }
}
