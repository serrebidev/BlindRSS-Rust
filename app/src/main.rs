//! BlindRSS (Rust) — main window over the existing Python database.
//! Slice 2: Refresh All (worker-thread fetch, main-thread store),
//! feed filtering via the tree, mark-read on open.
//! Screen-reader behaviour is NOT yet verified (manual NVDA/JAWS pass
//! is a Stage 0/6 acceptance item); layout follows BlindFB's proven
//! patterns: sizer-based, labelled controls, status bar announcements.

mod dialogs;
mod help;
mod hotkey;
mod keys;
mod player;
mod settings;
mod startup;

use blindrss_core::feeds::fetch_and_parse;
use blindrss_core::{Article, Db, Feed, ParsedFeed, store_parsed};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use wxdragon::event::webview_events::WebViewEvents;
use wxdragon::prelude::*;
use wxdragon::widgets::webview::{WebView, WebViewBackend};

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
/// Full-text arbitration (PARITY 7.6): when the active
/// provider is Miniflux and the article is one of its entries,
/// the server's fetch-content result competes with the local
/// cascade outcome; the more complete text wins. Runs on the
/// full-text worker thread.
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

/// Start (or replace) the AirPlay cast of `url` (an http(s)
/// URL or local file) to the RAOP device. ffmpeg decodes to
/// PCM; the RAOP sender streams it with the configured EQ.
fn start_raop_cast(
    device: &blindrss_core::cast::CastDevice,
    title: &str,
    url: &str,
    tx: &Sender<RefreshMsg>,
) -> Result<String, String> {
    // Stop any cast already running.
    if let Some(old) = raop_stop_registry().lock().unwrap().take() {
        old.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    *raop_stop_registry().lock().unwrap() = Some(stop.clone());
    let ffmpeg =
        blindrss_core::silence::find_ffmpeg().ok_or("AirPlay casting needs ffmpeg (not found)")?;
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let eq = blindrss_core::equalizer::EqConfig::from_config(&cfg);
    let (host, port) = (device.host.clone(), device.port);
    let url = url.to_string();
    let tx = tx.clone();
    let name = device.name.clone();
    let title_thread = title.to_string();
    let title = title.to_string();
    std::thread::spawn(move || {
        let title = title_thread;
        let mut child = match std::process::Command::new(&ffmpeg)
            .args([
                "-v", "error", "-i", &url, "-f", "s16le", "-ar", "44100", "-ac", "2", "-",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(RefreshMsg::StatusNote(format!(
                    "AirPlay cast failed to start ffmpeg: {e}"
                )));
                return;
            }
        };
        let mut pcm = child.stdout.take();
        let result = match pcm.as_mut() {
            Some(p) => blindrss_core::raop::stream_pcm(&host, port, p, Some(&eq), 80, stop.clone()),
            None => Err("ffmpeg gave no output".to_string()),
        };
        let _ = child.kill();
        let note = match result {
            Ok(_) => format!("AirPlay cast to {name} finished: {title}"),
            Err(e) => format!("AirPlay cast to {name} ended: {e}"),
        };
        if raop_stop_registry()
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| std::sync::Arc::ptr_eq(s, &stop))
            .unwrap_or(false)
        {
            *raop_stop_registry().lock().unwrap() = None;
        }
        let _ = tx.send(RefreshMsg::StatusNote(note));
    });
    Ok(format!("Casting {title} to {} (AirPlay)", device.name))
}

fn cast_to_device(
    device: &blindrss_core::cast::CastDevice,
    article: &Article,
    url: &str,
    tx: &Sender<RefreshMsg>,
) -> Result<String, String> {
    let mime = article
        .media_type
        .clone()
        .filter(|t| t.contains('/'))
        .unwrap_or_else(|| "audio/mpeg".to_string());
    if device.kind == "airplay" {
        // RAOP streams from us: no URL handoff. Audio only —
        // a video article casts its soundtrack.
        return start_raop_cast(device, &article.title, url, tx);
    }
    if device.kind == "chromecast" {
        let mut session =
            blindrss_core::castsession::CastSession::connect(&device.host, device.port)?;
        session.launch_default_receiver()?;
        session.load_media(url, &mime, &article.title)?;
        return Ok(format!("Casting {} to {}", article.title, device.name));
    }
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    let reachable = if url.starts_with("http://") || url.starts_with("https://") {
        blindrss_core::proxy::stream_url(url)
    } else {
        blindrss_core::proxy::file_url(std::path::Path::new(url))
    };
    blindrss_core::castupnp::play(
        device,
        &reachable,
        &article.title,
        &mime,
        cfg.get_str("cast_kodi_user", ""),
        cfg.get_str("cast_kodi_password", ""),
    )?;
    Ok(format!("Casting {} to {}", article.title, device.name))
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

/// Read/favorite writes route to the recommendations table for
/// virtual-feed articles (their ids are not in `articles`).
fn mark_article_read(db: &blindrss_core::db::Db, article: &blindrss_core::db::Article, read: bool) {
    if article.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
        let _ = db.set_recommendation_state(&article.id, Some(read), None);
    } else {
        let _ = db.mark_read(&article.id, read);
    }
}

fn mark_article_favorite(
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

/// Mirror a category rename/delete to the Miniflux server when
/// Miniflux is the active provider (Python's provider methods);
/// a refusal surfaces as a status note rather than a phantom
/// local-only success.
fn mirror_miniflux_category(
    tx: &std::sync::mpsc::Sender<RefreshMsg>,
    rename: Option<(String, String)>,
    delete: Option<String>,
) {
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

fn propagate_read_state(article: &blindrss_core::db::Article, read: bool) {
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

/// Apply a staged update (maintenance::download_and_stage_
/// update) before anything else starts: the staged tree
/// replaces the executable's folder contents. The running
/// binary is renamed aside first (Windows allows renaming a
/// running exe; the old copy is cleaned up on a later start),
/// then the staged files are copied over.
fn apply_pending_update() {
    let pending = data_dir().join("update_pending");
    let staged_root = pending.join("BlindRSS");
    let exe_name = if cfg!(target_os = "windows") {
        "blindrss.exe"
    } else {
        "blindrss"
    };
    let staged_exe = staged_root.join(exe_name);
    if !staged_exe.exists() {
        return;
    }
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let Some(exe_dir) = current.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    // Clean a previous swap's leftovers.
    let old = exe_dir.join("blindrss.old");
    if old.exists() {
        let _ = std::fs::remove_file(&old);
    }
    let _ = std::fs::rename(&current, &old);
    // Copy the whole staged tree over the install folder.
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        if let Ok(rd) = std::fs::read_dir(from) {
            for entry in rd.flatten() {
                let dest = to.join(entry.file_name());
                let path = entry.path();
                if path.is_dir() {
                    let _ = std::fs::create_dir_all(&dest);
                    copy_tree(&path, &dest);
                } else {
                    let _ = std::fs::copy(&path, &dest);
                }
            }
        }
    }
    copy_tree(&staged_root, &exe_dir);
    let _ = std::fs::remove_dir_all(&pending);
}

/// Apply the fetch-identity config to the shared HTTP agent
/// and to the environment yt-dlp reads (cookies file + UA).
/// Called at startup and after Settings changes.
fn apply_http_config() {
    let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
    blindrss_core::http::init_from_config(&cfg);
    blindrss_core::dependency::init_tool_paths_from_config(&cfg);
    // SAFETY: called on the UI thread before workers that read
    // these variables are spawned for a new action; this is the
    // same trade-off the process env always carries.
    unsafe {
        std::env::set_var(
            "BLINDRSS_YTDLP_COOKIES",
            cfg.get_str("ytdlp_cookies_file", ""),
        );
        std::env::set_var("BLINDRSS_USER_AGENT", blindrss_core::http::user_agent());
        // Managed yt-dlp (dependency self-update installs into
        // <data_dir>/bin) wins over PATH when present and the
        // user has not pinned their own binary.
        if std::env::var_os("BLINDRSS_YTDLP").is_none() {
            let managed = blindrss_core::dependency::managed_ytdlp(&data_dir());
            if managed.exists() {
                std::env::set_var("BLINDRSS_YTDLP", &managed);
            }
        }
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
            if now - last >= 86_400.0 {
                let raw_dir = cfg.get_str("auto_backup_dir", "").trim().to_string();
                let dir = if raw_dir.is_empty() {
                    data_dir().join("backups")
                } else {
                    PathBuf::from(raw_dir)
                };
                if std::fs::create_dir_all(&dir).is_ok() {
                    // Local-time-free stamp: seconds since epoch
                    // keeps names sortable without a date crate.
                    let stamp = format!("{}", now as i64);
                    let dest = dir.join(format!("BlindRSS-auto-backup-{stamp}.zip"));
                    if blindrss_core::maintenance::create_backup(&db_path(), &config_path(), &dest)
                        .is_ok()
                    {
                        let mut autos: Vec<PathBuf> = std::fs::read_dir(&dir)
                            .map(|rd| {
                                rd.filter_map(|e| e.ok().map(|e| e.path()))
                                    .filter(|p| {
                                        p.file_name()
                                            .map(|n| {
                                                n.to_string_lossy()
                                                    .starts_with("BlindRSS-auto-backup-")
                                            })
                                            .unwrap_or(false)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        autos.sort();
                        while autos.len() > 7 {
                            let old = autos.remove(0);
                            let _ = std::fs::remove_file(old);
                        }
                        let mut cfg2 =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        cfg2.set_path("auto_backup_last", serde_json::json!(now));
                        let _ = cfg2.save(&config_path());
                    }
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

/// Open an article link using the configured method (Python
/// `_open_article_url`, issue #31): with the "custom" method, run
/// the user's command template (%1 = URL); on any failure, warn
/// and fall back to the default browser so the user still gets
/// their article.
/// The rich reader's document skeleton: the WebView owns the
/// document (Python's design), the cleaned fragment drops into the
/// body, and the fragment's `<article lang="...">` carries the
/// content language for assistive tech.
fn rich_document(fragment: &str) -> String {
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
    /// Category tree expansion memory (Python issue #33): the
    /// user's explicit expand/collapse choices, re-applied across
    /// rebuilds; category_tree_default_expanded covers the rest.
    expanded_cats: std::collections::HashSet<String>,
    collapsed_cats: std::collections::HashSet<String>,
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
}

enum RefreshMsg {
    FeedDone(Feed, Result<ParsedFeed, String>),
    AllDone,
    /// Result of adding a feed by URL: (url, fetch result).
    Added(String, Result<(String, ParsedFeed), String>, Option<String>),
    /// Result of a hosted-provider sync: (provider name, (feeds, new)).
    ProviderDone(String, Result<(usize, usize), String>),
    /// Fetched article page HTML for full-text extraction.
    FullText(
        String,
        blindrss_core::extract::ExtractOutcome,
        Option<String>,
    ),
    /// Rich (WebView) full article: (article id, fragment if any).
    RichHtml(String, Option<String>),
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
    /// Cast discovery finished: the devices found plus the
    /// article and its resolved stream URL, for the picker.
    CastDevices(Vec<blindrss_core::cast::CastDevice>, Article, String),
    /// Update check: latest release tag.
    UpdateChecked(Result<(String, Option<String>), String>),
    /// Open-article-by-URL result: (page title, plain text).
    OpenedArticle(Result<(String, String), String>),
    /// A downloaded + verified update awaiting a restart.
    UpdateStaged(Result<String, String>),
    /// A finished active-provider action (hosted refresh,
    /// subscribe, unsubscribe): a human summary or an error.
    ProviderAction(Result<String, String>),
    /// A finished article translation: (article id, result).
    Translated(String, Result<String, String>),
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
            .map(|f| db.unread_count(Some(&f.id)).unwrap_or(0))
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
    // provider wrapper injects it whenever signed in).
    let yt_token = blindrss_core::config::Config::load(&config_path())
        .unwrap_or_default()
        .get_str("youtube_account_refresh_token", "")
        .to_string();
    if !yt_token.is_empty() {
        tree.append_item_with_data(
            &root,
            "Recommendations",
            blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID.to_string(),
            None,
            None,
        );
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

fn refill_articles(state: &mut State, list: &ListCtrl) {
    setup_article_columns(list);
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
                let tags_map = db.all_article_tags().unwrap_or_default();
                let opened_ids = db.opened_article_ids().unwrap_or_default();
                let updated_ids = db.updated_article_ids().unwrap_or_default();
                let empty_tags: Vec<String> = Vec::new();
                let matched: Vec<Article> = all
                    .into_iter()
                    .filter(|a| {
                        let view = blindrss_core::rules::RuleArticle {
                            article: a,
                            feed_title: feed_titles.get(a.feed_id.as_str()).copied().unwrap_or(""),
                            opened: opened_ids.contains(&a.id),
                            updated: updated_ids.contains(&a.id),
                            tags: tags_map.get(&a.id).unwrap_or(&empty_tags),
                        };
                        blindrss_core::rules::rule_matches(&rule, &view)
                    })
                    .collect();
                loaded = matched;
            }
        }
        return finish_refill(state, list, loaded);
    }
    // YouTube Recommendations virtual feed.
    if filter == Some(blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID) {
        let token = blindrss_core::config::Config::load(&config_path())
            .unwrap_or_default()
            .get_str("youtube_account_refresh_token", "")
            .to_string();
        let account = blindrss_core::youtube_account::account_key(&token);
        let loaded = db.recommendations(&account).unwrap_or_default();
        return finish_refill(state, list, loaded);
    }
    // Category selection: Python's membership clause — subtree
    // feeds, minus articles moved elsewhere, plus articles moved
    // or labeled into the subtree.
    if let Some(path) = filter.and_then(|f| f.strip_prefix("cat:")) {
        let loaded = db.articles_for_category(path, 500).unwrap_or_default();
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
    let layout = blindrss_core::article_columns::layout_from_config(
        &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
    );
    let visible = blindrss_core::article_columns::visible_columns(&layout);
    for (i, a) in loaded.iter().enumerate() {
        list.insert_item(i as i64, &a.title, None);
        for (col_idx, col) in visible.iter().enumerate().skip(1) {
            let cell = article_cell(a, &col.key, &feed_titles);
            list.set_item_text_by_column(i as i64, col_idx as i32, &cell);
        }
    }
    state.articles = loaded;
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
    // Release layout: locale/ ships beside the executable.
    let beside_exe = app_dir().join("locale");
    if beside_exe.exists() {
        return beside_exe;
    }
    // Dev fallback: the Python checkout's locale tree.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../blindrss-work/locale")
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
    let markers_before = cfg.raw().get("site_cookies_profile_mtimes").cloned();
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
    if cfg.raw().get("site_cookies_profile_mtimes").cloned() != markers_before {
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
        apply_pending_update();
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
        let mut tray_menu = Menu::builder()
            .append_item(TRAY_SHOW, "Show BlindRSS", "")
            .append_item(TRAY_REFRESH, "Refresh All", "")
            .append_separator()
            .append_item(TRAY_EXIT, "Exit", "")
            .build();
        tray.set_popup_menu(&mut tray_menu);
        // Set once the real actions exist (below); the tray menu
        // and close handler consult these slots.
        type ActionSlot = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
        let tray_refresh_slot: ActionSlot = Rc::new(RefCell::new(None));
        let quit_flag = Rc::new(std::cell::Cell::new(false));
        {
            // wx has no taskbar mouse events on macOS; the tray
            // menu's Show item is the restore path there.
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                let frame_tray = frame;
                tray.on_left_double_click(move |_| {
                    frame_tray.show(true);
                    frame_tray.raise();
                });
            }
        }
        {
            let frame_tray = frame;
            let slot = tray_refresh_slot.clone();
            let quit = quit_flag.clone();
            tray.on_menu(move |ev| match ev.get_id() {
                x if x == TRAY_SHOW => {
                    frame_tray.show(true);
                    frame_tray.raise();
                }
                x if x == TRAY_REFRESH => {
                    if let Some(action) = slot.borrow().as_ref() {
                        action();
                    }
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

        let (tx, rx): (Sender<RefreshMsg>, Receiver<RefreshMsg>) = channel();
        let splitter = SplitterWindow::builder(&frame).build();
        let tree = TreeCtrl::builder(&splitter).build();
        let right = Panel::builder(&splitter).build();
        let list = ListCtrl::builder(&right)
            .with_style(ListCtrlStyle::Report)
            .build();
        let reader = TextCtrl::builder(&right)
            .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
            .build();

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
            pending_play: None,
            pending_queue: None,
            refresh_new: Vec::new(),
            refresh_failed: false,
            search_focused: false,
            expanded_cats: std::collections::HashSet::new(),
            collapsed_cats: std::collections::HashSet::new(),
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

        // Selecting an article shows it and marks it read (Python marks
        // read on open as well).
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
            st.selected_article = Some(idx as usize);
            st.focus_list = true;
            let Some(a) = st.articles.get(idx as usize).cloned() else {
                return;
            };
            if let Some(db) = st.db.as_ref() {
                let _ = db.mark_opened(&a.id);
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
            if !a.is_read {
                if let Some(db) = st.db.as_ref() {
                    mark_article_read(db, &a, true);
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
        ) {
            // Refresh engine knobs (Audit 3): worker pool size,
            // per-host cap, retry attempts — read once per run.
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            let workers = cfg.get_i64("max_concurrent_refreshes", 16).clamp(1, 64) as usize;
            let per_host = cfg.get_i64("per_host_max_connections", 4).clamp(1, 64) as usize;
            let attempts = cfg.get_i64("feed_retry_attempts", 1).clamp(1, 10) as u32;
            let total = feeds.len();
            if total == 0 {
                let _ = tx.send(RefreshMsg::AllDone);
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
                std::thread::spawn(move || {
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
                        let mut result = fetch_and_parse(&feed.url);
                        for _ in 1..attempts {
                            if result.is_ok() {
                                break;
                            }
                            result = fetch_and_parse(&feed.url);
                        }
                        gate.release();
                        let _ = tx.send(RefreshMsg::FeedDone(feed, result));
                    }
                    if workers_left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                        let _ = tx.send(RefreshMsg::AllDone);
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
        }
        let rx = Rc::new(RefCell::new(rx));
        let state_btn = state.clone();
        let frame_btn = frame;
        let tx_refresh = tx.clone();
        let act_refresh: Rc<dyn Fn()> = Rc::new(move || {
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
                        st.refreshing = true;
                    }
                    frame_btn.set_status_text("Syncing provider...", 0);
                    announce("start_update", "Refresh Feeds");
                    let tx = tx_refresh.clone();
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                        let _ = tx.send(RefreshMsg::ProviderAction(provider.refresh()));
                    });
                    return;
                }
            }
            let (feeds, cancel, stopped) = {
                let mut st = state_btn.borrow_mut();
                if st.refreshing {
                    return;
                }
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
            spawn_refresh(feeds, tx_refresh.clone(), cancel, stopped);
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
                spawn_refresh(vec![feed], tx_rf.clone(), cancel, stopped);
            })
        };

        // Article actions: delete (tombstoned), favorite, mark all
        // read, restore-deleted dialog, and search.
        let state_del = state.clone();
        let frame_del = frame;
        let act_delete: Rc<dyn Fn()> = Rc::new(move || {
            let a = {
                let st = state_del.borrow();
                let Some(idx) = st.selected_article else {
                    frame_del.set_status_text("Select an article first", 0);
                    return;
                };
                let Some(a) = st.articles.get(idx).cloned() else {
                    return;
                };
                a
            };
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            // Delete behaviour: the feed's override wins over the
            // global setting ("deleted" tombstone, "purge", or
            // "category:<path>" move-instead-of-delete).
            let behavior = {
                let st = state_del.borrow();
                st.db
                    .as_ref()
                    .and_then(|db| db.feed_delete_behavior(&a.feed_id).ok().flatten())
                    .unwrap_or_else(|| cfg.get_str("delete_behavior", "deleted").to_string())
            };
            if let Some(path) = behavior.strip_prefix("category:") {
                let mut st = state_del.borrow_mut();
                if let Some(db) = st.db.as_ref() {
                    let _ = db.set_article_category_override(&a.id, Some(path));
                }
                refill_articles(&mut st, &list);
                frame_del.set_status_text(&format!("Moved {} to {path}", a.title), 0);
                return;
            }
            if cfg.get_bool("confirm_article_delete", true) {
                let md = MessageDialog::builder(
                    &frame_del,
                    &format!("Delete '{}'?", a.title),
                    "Delete Article",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build();
                let answer = md.show_modal();
                md.destroy();
                if answer != ID_YES {
                    restore_focus(&state_del.borrow(), &tree, &list);
                    return;
                }
            }
            let mut st = state_del.borrow_mut();
            if let Some(db) = st.db.as_ref() {
                if a.feed_id == blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
                    let _ = db.delete_recommendation(&a.id);
                } else if behavior == "purge" {
                    let _ = db.purge_article(&a.id);
                } else {
                    let _ = db.delete_article(&a.id);
                }
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
                mark_article_favorite(db, &a, new_value);
            }
            if let Some(slot) = st.articles.get_mut(idx) {
                slot.is_favorite = new_value;
            }
            // Hosted backend: favorites are the server's
            // starred/bookmarked state — push through the
            // active provider (cache write is idempotent).
            // Recommendations are local-only.
            if a.feed_id != blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                if blindrss_core::provider::active_name(&cfg) != "local" {
                    let a2 = a.clone();
                    std::thread::spawn(move || {
                        let cfg =
                            blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                        let provider = blindrss_core::provider::get_provider(&cfg, db_path());
                        let _ = provider.set_favorite(&a2, new_value);
                    });
                }
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
            let title_only = blindrss_core::config::Config::load(&config_path())
                .map(|c| c.get_str("search_mode", "title_content") == "title_only")
                .unwrap_or(false);
            match db.search_articles_mode(query.trim(), 300, title_only) {
                Ok(results) => {
                    setup_article_columns(&list);
                    let feed_titles: std::collections::HashMap<&str, &str> = st
                        .feeds
                        .iter()
                        .map(|f| (f.id.as_str(), f.title.as_str()))
                        .collect();
                    let layout = blindrss_core::article_columns::layout_from_config(
                        &blindrss_core::config::Config::load(&config_path()).unwrap_or_default(),
                    );
                    let visible = blindrss_core::article_columns::visible_columns(&layout);
                    for (i, a) in results.iter().enumerate() {
                        list.insert_item(i as i64, &a.title, None);
                        for (col_idx, col) in visible.iter().enumerate().skip(1) {
                            let cell = article_cell(a, &col.key, &feed_titles);
                            list.set_item_text_by_column(i as i64, col_idx as i32, &cell);
                        }
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
                        let page_size = blindrss_core::providers::page_size_from_config(&cfg);
                        let result = Db::open(&path).map_err(|e| format!("{e}")).and_then(|db| {
                            blindrss_core::providers::GReaderClient::with_oauth(
                                blindrss_core::providers::GReaderService::Inoreader,
                                &token,
                            )
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
            let keep_alt = {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                show_image_alt_for(&cfg, st.db.as_ref(), &article.feed_id)
            };
            let tx = tx_ft.clone();
            let id = article.id.clone();
            let url = article.url.clone();
            let article_feed = article.feed_id.clone();
            let article_content = article.content.clone();
            let article_id = article.id.clone();
            std::thread::spawn(move || {
                // YouTube articles use the dedicated full-text path
                // (description + chapters + transcript), like Python.
                if url.contains("youtube.com") || url.contains("youtu.be") {
                    let result = blindrss_core::download::youtube_full_text(&url);
                    let _ = tx.send(RefreshMsg::YoutubeText(id, result));
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
                    let _ = tx.send(RefreshMsg::FullText(id, outcome, first_html));
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
            let tx = tx_tl.clone();
            let id = article.id.clone();
            std::thread::spawn(move || {
                let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
                let result = blindrss_core::translate::translate_text(&cfg, &text, &target);
                let _ = tx.send(RefreshMsg::Translated(id, result));
            });
        });

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
                mark_article_read(db, &article, new_state);
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
        let tx_rc = tx.clone();
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
            if is_uncategorized_cat(Some(&category)) {
                frame_rc.set_status_text("The Uncategorized folder cannot be renamed.", 0);
                return;
            }
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
            drop(st);
            mirror_miniflux_category(
                &tx_rc,
                Some((category.clone(), new_name.trim().to_string())),
                None,
            );
            let mut st = state_rc.borrow_mut();
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
                    let url = maybe_cache_youtube(&article, &r.url, r.via_ytdlp);
                    (url, info)
                });
                let _ = tx.send(RefreshMsg::MediaReady(id, result));
            });
        });
        {
            let a = act_play.clone();
            play_btn.on_click(move |_| a());
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
            let changed = {
                let st = state_fp.borrow();
                match st.db.as_ref() {
                    Some(db) => dialogs::show_feed_properties(&frame_fp, db, &feed),
                    None => false,
                }
            };
            if changed {
                let mut st = state_fp.borrow_mut();
                st.feeds = st
                    .db
                    .as_ref()
                    .and_then(|db| db.feeds().ok())
                    .unwrap_or_default();
                rebuild_tree(&mut st, &tree_fp);
                refill_articles(&mut st, &list_fp);
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

        // Open Article from URL (Tools): fetch + extract on a
        // worker, then show the ArticleWindow from the drain.
        let frame_oa = frame;
        let state_oa = state.clone();
        let tree_oa = tree;
        let list_oa = list;
        let tx_oa = tx.clone();
        let act_open_article: Rc<dyn Fn()> = Rc::new(move || {
            let Some(url) = prompt_focus(
                &frame_oa,
                "Article URL:",
                "Open Article from URL",
                "",
                &state_oa.borrow(),
                &tree_oa,
                &list_oa,
            ) else {
                return;
            };
            let url = url.trim().to_string();
            if url.is_empty() {
                return;
            }
            frame_oa.set_status_text("Fetching article...", 0);
            let tx = tx_oa.clone();
            std::thread::spawn(move || {
                let jar = blindrss_core::cookies::load_jar_beside(&config_path());
                let header = blindrss_core::cookies::cookie_header_for(&jar, &url);
                let (outcome, _) = blindrss_core::extract::fetch_full_text(&url, &header, None, "");
                let result = match outcome {
                    blindrss_core::extract::ExtractOutcome::Article { title, text, .. } => {
                        Ok((title, text))
                    }
                    blindrss_core::extract::ExtractOutcome::Gate { reason } => {
                        Err(format!("page blocked: {reason}"))
                    }
                    blindrss_core::extract::ExtractOutcome::Empty => {
                        Err("nothing extractable on that page".to_string())
                    }
                };
                let _ = tx.send(RefreshMsg::OpenedArticle(result));
            });
        });

        // Open Media URL (Tools): play an arbitrary media URL in
        // the player.
        let frame_om = frame;
        let state_om = state.clone();
        let tree_om = tree;
        let list_om = list;
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
            player::open_player(&frame_om, article, url, Vec::new());
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
                        std::thread::spawn(move || {
                            let id = article.id.clone();
                            let resolved = blindrss_core::media::resolve_for_play(
                                article.media_url.as_deref(),
                                article.media_type.as_deref(),
                                &article.url,
                                article.content.as_deref(),
                            )
                            .map(|r| {
                                let info = if r.via_ytdlp {
                                    blindrss_core::media::info(&article.url).ok()
                                } else {
                                    None
                                };
                                (r.url, info)
                            });
                            let _ = tx.send(RefreshMsg::MediaReady(id, resolved));
                        });
                    }
                    Some(("subscribe", result)) => {
                        let url = result.url.clone();
                        frame_vs.set_status_text(&format!("Adding {url}..."), 0);
                        let tx = tx_vs.clone();
                        std::thread::spawn(move || {
                            let resolved = blindrss_core::discovery::resolve_subscribe_url(&url);
                            let _ = tx.send(RefreshMsg::Added(url, resolved, None));
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

        // Article Window (Article menu): the selected article in
        // its own window.
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
            dialogs::show_article_window(&article.title, &body);
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
                    .and_then(|db| blindrss_core::archive::rescan_archive(&db, &feed));
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
            std::thread::spawn(move || {
                let id = article.id.clone();
                let result = blindrss_core::media::resolve_for_play(
                    article.media_url.as_deref(),
                    article.media_type.as_deref(),
                    &article.url,
                    article.content.as_deref(),
                )
                .map(|r| {
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
                let result = blindrss_core::discovery::resolve_subscribe_url(&url);
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
            // downloads_enabled gates the feature (Python default
            // is off; the Settings Downloads section turns it on).
            if !cfg.get_bool("downloads_enabled", false) {
                frame_dl.set_status_text("Downloads are disabled — enable them in Settings", 0);
                return;
            }
            let dl_format = cfg.get_str("download_format", "video_best").to_string();
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
                            &dl_format,
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
                let result =
                    (|| -> Result<(Vec<blindrss_core::cast::CastDevice>, String), String> {
                        let resolved = blindrss_core::media::resolve_for_play(
                            article.media_url.as_deref(),
                            article.media_type.as_deref(),
                            &article.url,
                            article.content.as_deref(),
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
                        Ok((devices, resolved.url))
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
        let act_stop_cast: Rc<dyn Fn()> =
            Rc::new(move || match raop_stop_registry().lock().unwrap().take() {
                Some(flag) => {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    frame_sac.set_status_text("AirPlay cast stopping", 0);
                }
                None => frame_sac.set_status_text("No AirPlay cast is running", 0),
            });

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
        let act_settings: Rc<dyn Fn()> = Rc::new(move || {
            #[cfg(target_os = "windows")]
            let prior_accel = blindrss_core::config::Config::load(&config_path())
                .map(|c| {
                    c.get_str("global_show_hide_hotkey", "Ctrl+Alt+B")
                        .to_string()
                })
                .unwrap_or_else(|_| "Ctrl+Alt+B".to_string());
            let intent = settings::show_settings(&frame_set, &config_path());
            apply_http_config();
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
                // Discovery chain: social transforms, direct feed,
                // then HTML alternate links.
                let result = blindrss_core::discovery::resolve_subscribe_url(&url);
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
            let tx_dc = tx.clone();
            Rc::new(move || {
                let Some(path) = selected_category(&state_dc.borrow()) else {
                    frame_dc.set_status_text("Select a category in the tree first", 0);
                    return;
                };
                if is_uncategorized_cat(Some(&path)) {
                    frame_dc.set_status_text("The Uncategorized folder cannot be removed.", 0);
                    return;
                }
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
                let deleted = st.db.as_ref().map(|db| db.delete_category_path(&path));
                if matches!(deleted, Some(Ok(true))) {
                    mirror_miniflux_category(&tx_dc, None, Some(path.clone()));
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
                if is_uncategorized_cat(Some(&path)) {
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
                    let prefix = format!("{path}/");
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
                        mirror_miniflux_category(&tx_df, None, Some(sub.clone()));
                    }
                    let _ = db.delete_category_tree(&path);
                    mirror_miniflux_category(&tx_df, None, Some(path.clone()));
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

        // View Description (Python on_view_feed_description):
        // the selected article's feed-provided description in
        // its own window.
        let act_view_desc: Rc<dyn Fn()> = {
            let state_vd = state.clone();
            let frame_vd = frame;
            Rc::new(move || {
                let st = state_vd.borrow();
                let Some(a) = st
                    .selected_article
                    .and_then(|i| st.articles.get(i))
                    .cloned()
                else {
                    frame_vd.set_status_text("No article selected", 0);
                    return;
                };
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
                M_FEED_PROPS,
                "Feed &Properties...",
                "Title, URL and category for the selected feed",
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
            .append_item(
                M_ARTICLE_WINDOW,
                "Open in Article &Window",
                "Show the selected article in its own window",
            )
            .append_item(
                M_HISTORY,
                "View &History...",
                "Show the article's recorded change history",
            )
            .append_item(
                M_TRANSLATE,
                "&Translate Article",
                "Translate the article with the configured provider",
            )
            .append_item(
                M_QUEUE_TOGGLE,
                "Add to Play &Queue",
                "Add or remove the selected article from the play queue",
            )
            .append_item(
                M_QUEUE_OPEN,
                "Play &Queue...",
                "View and edit the play queue",
            )
            .append_item(M_QUEUE_NEXT, "Queue &Next", "Play the next queue entry")
            .append_item(
                M_QUEUE_PREV,
                "Queue Pre&vious",
                "Play the previous queue entry",
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
            .append_item(
                M_STOP_CAST,
                "Stop &AirPlay Cast",
                "Stop the running AirPlay cast",
            )
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
            .append_check_item(
                M_TOGGLE_SEARCH,
                "Show &Search Field",
                "Show or hide the search field",
            )
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
            .append_item(
                M_FILTER_RULES,
                "Filter &Rules...",
                "Rules that act on new articles",
            )
            .append_item(
                M_PERSISTENT,
                "&Persistent Search...",
                "Saved searches across all articles",
            )
            .append_item(
                M_OPEN_ARTICLE,
                "Open &Article from URL...",
                "Fetch and read a web article by URL",
            )
            .append_item(
                M_OPEN_MEDIA,
                "Open &Media URL...",
                "Play a media URL in the player",
            )
            .append_item(
                M_VIDEO_SEARCH,
                "&Video Search...",
                "Search videos across sites with yt-dlp",
            )
            .append_item(
                M_PODCAST_ARCHIVE,
                "Podcast &Archive...",
                "Recover old podcast episodes from the Wayback Machine",
            )
            .append_item(
                M_FEED_SEARCH,
                "&Find Feeds...",
                "Search a site for feeds to subscribe to",
            )
            .append_item(
                M_IMPORT_COOKIES,
                "Import Site &Cookies...",
                "Import browser cookies so challenge-protected sites can be read",
            )
            .build();
        let help_menu = Menu::builder()
            .append_item(M_HELP, "&User Guide", "Open the BlindRSS user guide")
            .append_item(M_ABOUT, "&About BlindRSS", "About this program")
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
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            if let Some(bar) = frame.get_menu_bar() {
                bar.check_item(M_TOGGLE_SEARCH, cfg.get_bool("show_search_field", true));
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
            (M_DELETE_ARTICLE, act_delete.clone()),
            (M_DELETED, act_deleted.clone()),
            (M_PLAY, act_play.clone()),
            (M_DOWNLOAD, act_download.clone()),
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
            (M_ABOUT, act_about.clone()),
            (M_ARTICLE_WINDOW, act_article_window.clone()),
            (M_PODCAST_ARCHIVE, act_podcast_archive.clone()),
            (M_QUEUE_TOGGLE, act_toggle_queue.clone()),
            (M_QUEUE_OPEN, act_queue_open.clone()),
            (M_QUEUE_NEXT, act_queue_next.clone()),
            (M_QUEUE_PREV, act_queue_prev.clone()),
            (M_FEED_SEARCH, act_feed_search.clone()),
            (M_TRANSLATE, act_translate.clone()),
            (M_COPY_URL, act_copy_url.clone()),
            (M_COPY_MEDIA, act_copy_media.clone()),
            (M_COPY_FEED_URL, act_copy_feed_url.clone()),
        ];
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
            }
        });

        // Context menus (PARITY 5C): right-click / Menu key on the
        // feed tree and the article list. Selections dispatch through
        // the frame's menu handler above (wx command events from a
        // popup propagate to the frame), so the same actions run.
        let state_ctx_tree = state.clone();
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
                .append_item(M_DEL_CAT_FEEDS, "Delete Category and Its &Feeds", "")
                .append_item(M_IMPORT_HERE, "Import OPML &Here...", "")
                .append_item(M_EXPORT_CAT, "&Export Category OPML...", "")
                .append_separator()
                .append_item(M_EDIT_FEED, "&Edit Feed...", "")
                .append_item(M_FEED_PROPS, "Feed &Properties...", "")
                .append_item(M_EDIT_SF, "Edit Smart &Folder...", "")
                .append_item(M_REMOVE_FEED, "Re&move Feed", "")
                .append_item(M_RENAME_CAT, "Rename &Category...", "")
                .append_item(M_SET_CAT, "Set Feed Categor&y...", "")
                .append_item(M_ARCHIVE, "Rescan &Archive", "")
                .append_item(M_COPY_FEED_URL, "Copy Feed &URL", "")
                .append_check_item(M_FEED_NOTIF, "&Notifications for This Feed", "")
                .append_separator()
                .append_item(M_NEW_SF, "&New Smart Folder...", "")
                .append_item(M_DEL_SF, "&Delete Smart Folder", "")
                .build();
            // Reflect the selection: the Notifications check
            // (checked = notifications ON for the selected feed)
            // and the Image Alt Text radio state.
            let (feed_id, db) = {
                let st = state_ctx_tree.borrow();
                (st.selected_feed.clone(), st.db.is_some())
            };
            if let Some(fid) = &feed_id {
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
                    .append_radio_item(M_IMGALT_DEFAULT, "Use &Default", "")
                    .append_radio_item(M_IMGALT_ALWAYS, "&Always", "")
                    .append_radio_item(M_IMGALT_NEVER, "&Never", "")
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
        list.bind_internal(EventType::CONTEXT_MENU, move |ev| {
            ev.skip(false);
            let mut menu = Menu::builder()
                .append_item(M_TOGGLE_READ, "Toggle &Read", "")
                .append_item(M_FULLTEXT, "Read &Full Text", "")
                .append_item(M_VIEW_DESC, "View &Description...", "")
                .append_item(M_HISTORY, "View &History...", "")
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
                Some((_, url)) if !url.is_empty() => open_article_link(&frame_enter, &url),
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
            let h_video_search = act_video_search.clone();
            let h_import_cookies = act_import_cookies.clone();
            let h_errors = act_feed_errors.clone();
            let h_toggle_read = act_toggle_read.clone();
            let h_fav = act_fav.clone();
            let h_toggle_queue = act_toggle_queue.clone();
            let h_queue_open = act_queue_open.clone();
            let h_queue_next = act_queue_next.clone();
            let h_queue_prev = act_queue_prev.clone();
            let h_play = act_play.clone();
            let h_f_all = act_filter_all.clone();
            let h_f_unread = act_filter_unread.clone();
            let h_f_read = act_filter_read.clone();
            let h_m_all = act_media_all.clone();
            let h_m_with = act_media_with.clone();
            let h_m_without = act_media_without.clone();
            let h_s_date = act_sort_date.clone();
            let h_toggle_search = act_toggle_search.clone();
            let h_s_name = act_sort_name.clone();
            let h_s_author = act_sort_author.clone();
            let h_s_feed = act_sort_feed.clone();
            let h_s_dir = act_sort_dir.clone();
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
                    Some("feeds.video_search") => {
                        h_video_search();
                        true
                    }
                    Some("tools.import_site_cookies") => {
                        h_import_cookies();
                        true
                    }
                    Some("article.toggle_favorite") => {
                        h_fav();
                        true
                    }
                    Some("article.toggle_queue") => {
                        h_toggle_queue();
                        true
                    }
                    Some("queue.open") => {
                        h_queue_open();
                        true
                    }
                    Some("queue.next") => {
                        h_queue_next();
                        true
                    }
                    Some("queue.prev") => {
                        h_queue_prev();
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
                    Some("view.toggle_search") => {
                        h_toggle_search();
                        true
                    }
                    _ => false,
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
        let state_tick = state.clone();
        let frame_tick = frame;
        let rich_view_tick = rich_view.clone();
        let rich_loading_tick = rich_loading.clone();
        let rx_tick = rx.clone();
        let tx_tick = tx.clone();
        timer.on_tick(move |_| {
            let mut new_total = 0usize;
            let mut finished = false;
            loop {
                let msg = rx_tick.borrow().try_recv();
                match msg {
                    Ok(RefreshMsg::FeedDone(feed, Ok(parsed))) => {
                        let mut st = state_tick.borrow_mut();
                        let mut stored_new = 0usize;
                        if let Some(db) = st.db.as_ref() {
                            match store_parsed(db, &feed, &parsed) {
                                Ok(n) => {
                                    new_total += n;
                                    stored_new = n;
                                    let _ = db.clear_feed_error(&feed.id);
                                }
                                Err(e) => frame_tick.set_status_text(
                                    &format!("{}: store failed: {e}", feed.title),
                                    0,
                                ),
                            }
                        }
                        if stored_new > 0 {
                            st.refresh_new
                                .push((feed.id.clone(), feed.title.clone(), stored_new));
                        }
                    }
                    Ok(RefreshMsg::FeedDone(feed, Err(e))) => {
                        let mut st = state_tick.borrow_mut();
                        st.refresh_failed = true;
                        if let Some(db) = st.db.as_ref() {
                            let _ = db.record_feed_error(&feed.id, &e);
                        }
                        frame_tick.set_status_text(&format!("{}: {e}", feed.title), 0);
                    }
                    Ok(RefreshMsg::Added(_url, Ok((feed_url, parsed)), add_category)) => {
                        let mut st = state_tick.borrow_mut();
                        if let Some(db) = st.db.as_ref() {
                            let title = if parsed.title.trim().is_empty() {
                                feed_url.clone()
                            } else {
                                parsed.title.clone()
                            };
                            match db.add_feed(
                                &feed_url,
                                &title,
                                add_category.as_deref().filter(|c| !c.trim().is_empty()),
                            ) {
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
                    Ok(RefreshMsg::Translated(id, result)) => {
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
                                frame_tick.set_status_text("Article translated", 0);
                            }
                            Err(e) => frame_tick.set_status_text(&format!("Translate: {e}"), 0),
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
                    Ok(RefreshMsg::FullText(id, outcome, first_html)) => {
                        let st = state_tick.borrow();
                        let article = st.articles.iter().find(|a| a.id == id).cloned();
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
                                match queue_ctx {
                                    Some((entries, index)) => player::open_player_queue(
                                        &frame_tick,
                                        article,
                                        stream,
                                        entries,
                                        index,
                                    ),
                                    None => {
                                        player::open_player(&frame_tick, article, stream, chapters)
                                    }
                                }
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
                    Ok(RefreshMsg::CastDevices(devices, article, url)) => {
                        let device = if devices.len() == 1 {
                            devices.into_iter().next()
                        } else {
                            dialogs::show_cast_picker(&frame_tick, &devices)
                        };
                        restore_focus(&state_tick.borrow(), &tree, &list);
                        if let Some(device) = device {
                            let tx2 = tx.clone();
                            std::thread::spawn(move || {
                                let result = cast_to_device(&device, &article, &url, &tx2);
                                let _ = tx2.send(RefreshMsg::CastDone(result));
                            });
                        }
                    }
                    Ok(RefreshMsg::CastDone(result)) => match result {
                        Ok(msg) => frame_tick.set_status_text(&msg, 0),
                        Err(e) => frame_tick.set_status_text(&format!("Cast: {e}"), 0),
                    },
                    Ok(RefreshMsg::UpdateStaged(result)) => match result {
                        Ok(tag) => frame_tick.set_status_text(
                            &format!("Update {tag} downloaded — restart BlindRSS to apply it"),
                            0,
                        ),
                        Err(e) => frame_tick.set_status_text(&format!("Update download: {e}"), 0),
                    },
                    Ok(RefreshMsg::OpenedArticle(result)) => match result {
                        Ok((title, text)) => {
                            dialogs::show_article_window(&title, &text);
                            frame_tick.set_status_text("Article opened in its own window", 0);
                        }
                        Err(e) => frame_tick.set_status_text(&format!("Open article: {e}"), 0),
                    },
                    Ok(RefreshMsg::UpdateChecked(result)) => match result {
                        Ok((tag, _)) => {
                            if tag.trim_start_matches('v') == env!("CARGO_PKG_VERSION") {
                                frame_tick.set_status_text("BlindRSS is up to date", 0);
                            } else {
                                frame_tick.set_status_text(&format!("Update available: {tag}"), 0);
                                let cfg = blindrss_core::config::Config::load(&config_path())
                                    .unwrap_or_default();
                                if cfg.get_bool("install_updates_automatically", false) {
                                    frame_tick.set_status_text(
                                        &format!("Downloading update {tag}..."),
                                        0,
                                    );
                                    let tx_stage = tx.clone();
                                    std::thread::spawn(move || {
                                        let result =
                                            blindrss_core::maintenance::download_and_stage_update(
                                                &data_dir(),
                                            );
                                        let _ = tx_stage.send(RefreshMsg::UpdateStaged(result));
                                    });
                                }
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
                let new_by_feed = std::mem::take(&mut st.refresh_new);
                let failed = std::mem::take(&mut st.refresh_failed);
                drop(st);
                refresh_side_effects(&tray, &new_by_feed, failed);
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
        {
            let cfg = blindrss_core::config::Config::load(&config_path()).unwrap_or_default();
            frame.show(!cfg.get_bool("start_in_system_tray", false));
        }
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
            // Startup update check (auto_check_updates, default
            // on): the UpdateChecked drain reports, and stages
            // the download when install_updates_automatically.
            if cfg.get_bool("auto_check_updates", true) {
                let update_timer = Box::leak(Box::new(Timer::new(&frame)));
                let update_start = act_update.clone();
                update_timer.on_tick(move |_| {
                    update_start();
                });
                update_timer.start(4000, true);
            }
        }
    });
}
