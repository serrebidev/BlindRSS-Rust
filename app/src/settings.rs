//! Settings dialog: provider accounts + download path.
//!
//! Writes into the shared Python-format config.json
//! (providers.theoldreader / bazqux / miniflux / inoreader,
//! download_path). Inoreader connect uses the paste-the-redirect
//! OAuth path (same as Python supports): open the auth page in the
//! browser, paste the final URL back, exchange the code.

use blindrss_core::config::Config;
use std::path::Path;
use wxdragon::prelude::*;

fn get_path(cfg: &Config, path: &str) -> String {
    let mut cur = cfg.raw();
    for part in path.split('.') {
        match cur.get(part) {
            Some(v) => cur = v,
            None => return String::new(),
        }
    }
    cur.as_str().unwrap_or("").to_string()
}

fn add_field(
    dialog: &Dialog,
    sizer: &BoxSizer,
    label: &str,
    value: &str,
    password: bool,
) -> TextCtrl {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(dialog).with_label(label).build();
    let style = if password {
        TextCtrlStyle::Password
    } else {
        TextCtrlStyle::default()
    };
    let ctrl = TextCtrl::builder(dialog).with_style(style).build();
    ctrl.set_value(value);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    ctrl
}

/// A labeled checkbox row; returns the checkbox.
fn add_check(dialog: &Dialog, sizer: &BoxSizer, label: &str, value: bool) -> CheckBox {
    let cb = CheckBox::builder(dialog).with_label(label).build();
    cb.set_value(value);
    sizer.add(&cb, 0, SizerFlag::All, 2);
    cb
}

/// A labeled choice row; returns the choice control.
fn add_choice(
    dialog: &Dialog,
    sizer: &BoxSizer,
    label: &str,
    items: &[&str],
    selected: usize,
) -> Choice {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(dialog).with_label(label).build();
    let choice = Choice::builder(dialog).build();
    for item in items {
        choice.append(item);
    }
    choice.set_selection(selected.min(items.len().saturating_sub(1)) as u32);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&choice, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    choice
}

/// A section heading.
fn add_heading(dialog: &Dialog, sizer: &BoxSizer, label: &str) {
    let text = StaticText::builder(dialog).with_label(label).build();
    sizer.add(&text, 0, SizerFlag::All, 4);
}

/// Fire a test desktop notification from Settings (platform
/// commands; the real path uses the tray balloon on Windows).
fn test_notify() {
    #[cfg(target_os = "windows")]
    {
        let script = "$n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(4000, 'BlindRSS', 'Test notification', [System.Windows.Forms.ToolTipIcon]::Info)";
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args([
                "-e",
                "display notification \"Test notification\" with title \"BlindRSS\"",
            ])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["BlindRSS", "Test notification"])
            .spawn();
    }
}

fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Inoreader sign-in through the local HTTPS listener
/// (Python's local-server OAuth variant): a self-signed
/// server on 127.0.0.1:18423 waits for the browser redirect
/// (the browser warns about the self-signed certificate —
/// the redirect target registered with Inoreader is this
/// https URL). The code is exchanged and the tokens saved,
/// exactly like the paste-the-URL flow.
fn inoreader_listener_connect(frame: &Dialog, config_path: &Path, app_id: String, app_key: String) {
    use std::io::{Read, Write};
    let state = format!("blindrss{}", std::process::id());
    let url = blindrss_core::oauth::authorization_url(
        &app_id,
        blindrss_core::oauth::DEFAULT_REDIRECT,
        &state,
    );
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let send = |r: Result<String, String>| {
            let _ = tx.send(r);
        };
        let cert = match rcgen::generate_simple_self_signed(vec![
            "127.0.0.1".to_string(),
            "localhost".to_string(),
        ]) {
            Ok(c) => c,
            Err(e) => return send(Err(format!("certificate: {e}"))),
        };
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
        let server_cfg = match rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
        {
            Ok(c) => std::sync::Arc::new(c),
            Err(e) => return send(Err(format!("tls config: {e}"))),
        };
        let listener = match std::net::TcpListener::bind("127.0.0.1:18423") {
            Ok(l) => l,
            Err(e) => return send(Err(format!("listener: {e}"))),
        };
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut result: Result<String, String> = Err("sign-in timed out".to_string());
        while std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
                    let mut conn = match rustls::ServerConnection::new(server_cfg.clone()) {
                        Ok(c) => c,
                        Err(e) => return send(Err(format!("tls: {e}"))),
                    };
                    let mut tls = rustls::Stream::new(&mut conn, &mut stream);
                    let mut buf = [0u8; 8192];
                    if let Ok(n) = tls.read(&mut buf) {
                        let request = String::from_utf8_lossy(&buf[..n]).to_string();
                        let page = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1>BlindRSS</h1><p>Inoreader sign-in received. You can close this tab.</p></body></html>";
                        let _ = tls.write_all(page.as_bytes());
                        let _ = tls.flush();
                        let (code, _) = blindrss_core::oauth::parse_oauth_redirect(&request);
                        result = match code {
                            Some(c) => Ok(c),
                            None => Err("the redirect carried no code".to_string()),
                        };
                    }
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => {
                    result = Err(format!("accept: {e}"));
                    break;
                }
            }
        }
        send(result);
    });
    open_url(&url);
    // Drain on a dialog timer like the other async flows.
    let dialog2 = Dialog::builder(frame, "Inoreader Sign-in").build();
    let sizer2 = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog2)
        .with_label(&crate::tr("Waiting for the browser sign-in to complete..."))
        .build();
    sizer2.add(&label, 0, SizerFlag::All, 8);
    dialog2.set_sizer(sizer2, true);
    dialog2.set_size(Size::new(440, 160));
    let timer = Box::leak(Box::new(Timer::new(&dialog2)));
    let path = config_path.to_path_buf();
    let frame_wait = *frame;
    timer.on_tick(move |_| {
        if let Ok(result) = rx.try_recv() {
            let d = dialog2;
            match result.and_then(|code| {
                blindrss_core::oauth::exchange_code(
                    &app_id,
                    &app_key,
                    &code,
                    blindrss_core::oauth::DEFAULT_REDIRECT,
                )
            }) {
                Ok(tokens) => {
                    let mut cfg2 = Config::load(&path).unwrap_or_default();
                    if let Some(t) = tokens.get("access_token") {
                        cfg2.set_path("providers.inoreader.token", t.clone());
                    }
                    if let Some(t) = tokens.get("refresh_token") {
                        cfg2.set_path("providers.inoreader.refresh_token", t.clone());
                    }
                    if let Some(exp) = tokens.get("expires_in").and_then(|v| v.as_i64()) {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        cfg2.set_path(
                            "providers.inoreader.token_expires_at",
                            serde_json::json!(now + exp),
                        );
                    }
                    cfg2.set_path("providers.inoreader.app_id", serde_json::json!(app_id));
                    cfg2.set_path("providers.inoreader.app_key", serde_json::json!(app_key));
                    let _ = cfg2.save(&path);
                    d.end_modal(5100);
                    let md =
                        MessageDialog::builder(&frame_wait, "Inoreader connected.", "Inoreader")
                            .with_style(
                                MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                            )
                            .build();
                    md.show_modal();
                }
                Err(e) => {
                    d.end_modal(5101);
                    let md = MessageDialog::builder(
                        &frame_wait,
                        &format!("Inoreader sign-in failed: {e}"),
                        "Inoreader",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                    .build();
                    md.show_modal();
                }
            }
        }
    });
    timer.start(200, false);
    dialog2.show_modal();
    dialog2.destroy();
}

/// Shows the settings dialog. Returns the name of a maintenance
/// action the user chose inside the dialog ("backup", "restore",
/// "takeout", "archive", "sync", "prune", "update", "columns"), or None. The
/// caller runs the action after the dialog has closed, so the
/// action's own dialogs parent cleanly on the main frame.
pub fn show_settings(frame: &Frame, config_path: &Path) -> Option<&'static str> {
    let cfg = Config::load(config_path).unwrap_or_default();
    let dialog = Dialog::builder(frame, "BlindRSS Settings").build();
    crate::dialogs::bind_f1(frame, &dialog, "SettingsDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Active provider (Python's Provider page + factory.py):
    // the backend the whole UI reads/writes through.
    add_heading(&dialog, &sizer, "Provider");
    let active_sel = blindrss_core::provider::PROVIDER_NAMES
        .iter()
        .position(|n| *n == cfg.get_str("active_provider", "local"))
        .unwrap_or(0);
    let active_choice = add_choice(
        &dialog,
        &sizer,
        "Active provider",
        &blindrss_core::provider::PROVIDER_LABELS,
        active_sel,
    );

    let tor_email = add_field(
        &dialog,
        &sizer,
        "The Old Reader email",
        &get_path(&cfg, "providers.theoldreader.email"),
        false,
    );
    let tor_pass = add_field(
        &dialog,
        &sizer,
        "The Old Reader password",
        &get_path(&cfg, "providers.theoldreader.password"),
        true,
    );
    let baz_email = add_field(
        &dialog,
        &sizer,
        "BazQux email",
        &get_path(&cfg, "providers.bazqux.email"),
        false,
    );
    let baz_pass = add_field(
        &dialog,
        &sizer,
        "BazQux password",
        &get_path(&cfg, "providers.bazqux.password"),
        true,
    );
    let mini_url = add_field(
        &dialog,
        &sizer,
        "Miniflux URL",
        &get_path(&cfg, "providers.miniflux.url"),
        false,
    );
    let mini_key = add_field(
        &dialog,
        &sizer,
        "Miniflux API key",
        &get_path(&cfg, "providers.miniflux.api_key"),
        true,
    );
    let ino_id = add_field(
        &dialog,
        &sizer,
        "Inoreader app ID",
        &get_path(&cfg, "providers.inoreader.app_id"),
        false,
    );
    let ino_key = add_field(
        &dialog,
        &sizer,
        "Inoreader app key",
        &get_path(&cfg, "providers.inoreader.app_key"),
        true,
    );
    let dl_path = add_field(
        &dialog,
        &sizer,
        "Download folder",
        &get_path(&cfg, "download_path"),
        false,
    );

    // Startup state (Python parity keys).
    let startup_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Startup"))
        .build();
    sizer.add(&startup_label, 0, SizerFlag::All, 4);
    let remember_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Remember last feed"))
        .build();
    remember_cb.set_value(cfg.get_bool("remember_last_feed", false));
    sizer.add(&remember_cb, 0, SizerFlag::All, 4);
    let maximized_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Start maximized"))
        .build();
    maximized_cb.set_value(cfg.get_bool("start_maximized", false));
    sizer.add(&maximized_cb, 0, SizerFlag::All, 4);
    let expand_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Expand categories by default"))
        .build();
    expand_cb.set_value(cfg.get_bool("category_tree_default_expanded", true));
    sizer.add(&expand_cb, 0, SizerFlag::All, 4);
    let hotkey_field = add_field(
        &dialog,
        &sizer,
        "Global show/hide hotkey (Windows, blank = off)",
        cfg.get_str("global_show_hide_hotkey", "Ctrl+Alt+B"),
        false,
    );
    // Start at login (Python windows_integration): the checkbox
    // mirrors the config key; saving applies the OS registration
    // when the value changed. Disabled where unsupported.
    let start_login_cb = add_check(
        &dialog,
        &sizer,
        crate::startup::setting_label(),
        cfg.get_bool("start_on_windows_login", false),
    );
    if !crate::startup::supported() {
        start_login_cb.enable(false);
    }

    // Refresh scheduling (Python keys: refresh_on_startup defaults
    // on, refresh_interval is in seconds, 300 default, 0 = off).
    let refresh_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Refresh"))
        .build();
    sizer.add(&refresh_label, 0, SizerFlag::All, 4);
    let refresh_startup_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Refresh feeds on startup"))
        .build();
    refresh_startup_cb.set_value(cfg.get_bool("refresh_on_startup", true));
    sizer.add(&refresh_startup_cb, 0, SizerFlag::All, 4);
    let interval_field = add_field(
        &dialog,
        &sizer,
        "Refresh interval (seconds, 0 = off)",
        &cfg.get_i64("refresh_interval", 300).to_string(),
        false,
    );

    // Announcements (PARITY 7.4): per-event speech/Braille mode,
    // mirroring Python's Settings > Announcements page.
    let announce_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Announcements"))
        .build();
    sizer.add(&announce_label, 0, SizerFlag::All, 4);
    let current_modes = blindrss_core::announce::Announcements::from_config(&cfg);
    let mut announce_choices: Vec<Choice> = Vec::new();
    for ev in blindrss_core::announce::EVENTS.iter() {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let lbl = StaticText::builder(&dialog).with_label(ev.label).build();
        row.add(&lbl, 1, SizerFlag::All, 4);
        let choice = Choice::builder(&dialog).build();
        for label in blindrss_core::announce::MODE_LABELS {
            choice.append(label);
        }
        choice.set_selection(current_modes.mode_index(ev.id) as u32);
        row.add(&choice, 0, SizerFlag::All, 4);
        sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
        announce_choices.push(choice);
    }
    let announce_test_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Test Announcement"))
        .build();
    sizer.add(&announce_test_btn, 0, SizerFlag::All, 4);

    // Refresh engine (Audit 3 keys).
    add_heading(&dialog, &sizer, "Refresh Engine");
    let max_conc_field = add_field(
        &dialog,
        &sizer,
        "Max concurrent refreshes",
        &cfg.get_i64("max_concurrent_refreshes", 16).to_string(),
        false,
    );
    let timeout_field = add_field(
        &dialog,
        &sizer,
        "Feed timeout (seconds)",
        &cfg.get_i64("feed_timeout_seconds", 15).to_string(),
        false,
    );
    let retries_field = add_field(
        &dialog,
        &sizer,
        "Feed retry attempts",
        &cfg.get_i64("feed_retry_attempts", 1).to_string(),
        false,
    );
    let per_host_field = add_field(
        &dialog,
        &sizer,
        "Max connections per host",
        &cfg.get_i64("per_host_max_connections", 4).to_string(),
        false,
    );
    let ignore_cache_cb = add_check(
        &dialog,
        &sizer,
        "Ignore feed cache",
        cfg.get_bool("ignore_feed_cache", false),
    );

    // Reading.
    add_heading(&dialog, &sizer, "Reading");
    let search_mode_choice = add_choice(
        &dialog,
        &sizer,
        "Search looks in",
        &["Titles and content", "Titles only"],
        if cfg.get_str("search_mode", "title_content") == "title_only" {
            1
        } else {
            0
        },
    );
    let delete_behavior0 = cfg.get_str("delete_behavior", "deleted").to_string();
    let delete_path0 = delete_behavior0
        .strip_prefix("category:")
        .unwrap_or("")
        .to_string();
    let delete_choice = add_choice(
        &dialog,
        &sizer,
        "Deleting an article",
        &[
            "Moves it to the Deleted view",
            "Deletes it permanently",
            "Moves it to a category instead",
        ],
        if delete_behavior0 == "purge" {
            1
        } else if delete_behavior0.starts_with("category:") {
            2
        } else {
            0
        },
    );
    let delete_path_field = add_field(
        &dialog,
        &sizer,
        "Category path for the move-instead option",
        &delete_path0,
        false,
    );
    let confirm_delete_cb = add_check(
        &dialog,
        &sizer,
        "Confirm before deleting an article",
        cfg.get_bool("confirm_article_delete", true),
    );
    let show_alt_cb = add_check(
        &dialog,
        &sizer,
        "Show image alt text",
        cfg.get_bool("show_image_alt", false),
    );
    // Article opening (Python issue #31): default browser or a
    // custom command template with %1 for the URL.
    let open_method_choice = add_choice(
        &dialog,
        &sizer,
        "Article opening method",
        &["Default browser", "Custom command"],
        if cfg
            .get_str("article_open_method", "default")
            .eq_ignore_ascii_case("custom")
        {
            1
        } else {
            0
        },
    );
    let open_cmd_field = add_field(
        &dialog,
        &sizer,
        "Custom command (%1 = article URL)",
        cfg.get_str("article_open_command", ""),
        false,
    );
    let open_cmd_test_btn = Button::builder(&dialog)
        .with_label("Test Custom Command")
        .build();
    sizer.add(&open_cmd_test_btn, 0, SizerFlag::All, 2);

    // External tools: user-chosen executable paths take the
    // highest detection priority (blank = auto-detect).
    add_heading(&dialog, &sizer, "External Tools");
    let ffmpeg_path_field = add_field(
        &dialog,
        &sizer,
        "ffmpeg path (blank = auto)",
        cfg.get_str("custom_ffmpeg_path", ""),
        false,
    );
    let ffprobe_path_field = add_field(
        &dialog,
        &sizer,
        "ffprobe path (blank = auto)",
        cfg.get_str("custom_ffprobe_path", ""),
        false,
    );
    let ytdlp_path_field = add_field(
        &dialog,
        &sizer,
        "yt-dlp path (blank = auto)",
        cfg.get_str("custom_ytdlp_path", ""),
        false,
    );

    // Downloads.
    add_heading(&dialog, &sizer, "Downloads");
    let downloads_enabled_cb = add_check(
        &dialog,
        &sizer,
        "Downloads enabled",
        cfg.get_bool("downloads_enabled", false),
    );
    let format_labels: Vec<&str> = blindrss_core::download::DOWNLOAD_FORMAT_IDS
        .iter()
        .map(|id| blindrss_core::download::download_format_label(id))
        .collect();
    let format_sel = blindrss_core::download::DOWNLOAD_FORMAT_IDS
        .iter()
        .position(|id| *id == cfg.get_str("download_format", "video_best"))
        .unwrap_or(0);
    let format_choice = add_choice(
        &dialog,
        &sizer,
        "Download format",
        &format_labels,
        format_sel,
    );
    let max_dl_field = add_field(
        &dialog,
        &sizer,
        "Max downloads kept in the queue",
        &cfg.get_i64("max_downloads", 32).to_string(),
        false,
    );
    let retention_labels: Vec<String> = blindrss_core::retention::RETENTION_CHOICES
        .iter()
        .map(|(id, _)| blindrss_core::retention::retention_label(id))
        .collect();
    let retention_refs: Vec<&str> = retention_labels.iter().map(|s| s.as_str()).collect();
    let art_ret_sel = blindrss_core::retention::RETENTION_CHOICES
        .iter()
        .position(|(id, _)| *id == cfg.get_str("article_retention", "unlimited"))
        .unwrap_or(11);
    let art_ret_choice = add_choice(
        &dialog,
        &sizer,
        "Keep articles for",
        &retention_refs,
        art_ret_sel,
    );
    let dl_ret_sel = blindrss_core::retention::RETENTION_CHOICES
        .iter()
        .position(|(id, _)| *id == cfg.get_str("download_retention", "unlimited"))
        .unwrap_or(11);
    let dl_ret_choice = add_choice(
        &dialog,
        &sizer,
        "Keep downloads for",
        &retention_refs,
        dl_ret_sel,
    );

    // Player / media.
    add_heading(&dialog, &sizer, "Player");
    let show_player_cb = add_check(
        &dialog,
        &sizer,
        "Show player when playback starts",
        cfg.get_bool("show_player_on_play", true),
    );
    let speed_field = add_field(
        &dialog,
        &sizer,
        "Playback speed (1.0 = normal)",
        &cfg.raw()
            .get("playback_speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0)
            .to_string(),
        false,
    );
    let seek_back_field = add_field(
        &dialog,
        &sizer,
        "Seek back (milliseconds)",
        &cfg.get_i64("seek_back_ms", 10_000).to_string(),
        false,
    );
    let seek_fwd_field = add_field(
        &dialog,
        &sizer,
        "Seek forward (milliseconds)",
        &cfg.get_i64("seek_forward_ms", 10_000).to_string(),
        false,
    );
    let vol_step_field = add_field(
        &dialog,
        &sizer,
        "Volume step (percent)",
        &cfg.get_i64("volume_step", 5).to_string(),
        false,
    );
    let sponsor_cb = add_check(
        &dialog,
        &sizer,
        "Skip sponsors (SponsorBlock)",
        cfg.get_bool("sponsorblock_enabled", true),
    );
    let sponsor_cats_field = add_field(
        &dialog,
        &sizer,
        "SponsorBlock categories (comma separated)",
        &cfg.raw()
            .get("sponsorblock_categories")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "sponsor, selfpromo, interaction".to_string()),
        false,
    );
    let silence_cb = add_check(
        &dialog,
        &sizer,
        "Skip silence in playback",
        cfg.get_bool("skip_silence", true),
    );
    let silence_db_field = add_field(
        &dialog,
        &sizer,
        "Silence threshold (dB)",
        &cfg.raw()
            .get("silence_skip_threshold_db")
            .and_then(|v| v.as_f64())
            .unwrap_or(-38.0)
            .to_string(),
        false,
    );
    let silence_ms_field = add_field(
        &dialog,
        &sizer,
        "Minimum silence (milliseconds)",
        &cfg.get_i64("silence_skip_min_ms", 700).to_string(),
        false,
    );

    // Equalizer (its own dialog, opened via the intent
    // mechanism like Article Columns).
    add_heading(&dialog, &sizer, "Equalizer");
    let eq_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Equalizer..."))
        .build();
    sizer.add(&eq_btn, 0, SizerFlag::All, 2);

    // Notifications + sounds.
    add_heading(&dialog, &sizer, "Notifications and Sounds");
    let notif_cb = add_check(
        &dialog,
        &sizer,
        "Desktop notifications for new articles",
        cfg.get_bool("windows_notifications_enabled", false),
    );
    let notif_name_cb = add_check(
        &dialog,
        &sizer,
        "Include the feed name in notifications",
        cfg.get_bool("windows_notifications_include_feed_name", true),
    );
    let notif_max_field = add_field(
        &dialog,
        &sizer,
        "Max notifications per refresh (0 = unlimited)",
        &cfg.get_i64("windows_notifications_max_per_refresh", 0)
            .to_string(),
        false,
    );
    let notif_summary_cb = add_check(
        &dialog,
        &sizer,
        "Show one summary when notifications are capped",
        cfg.get_bool("windows_notifications_show_summary_when_capped", true),
    );
    let notif_row = BoxSizer::builder(Orientation::Horizontal).build();
    let exclude_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Exclude Feeds..."))
        .build();
    let test_notif_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Test Notification"))
        .build();
    notif_row.add(&exclude_btn, 0, SizerFlag::All, 4);
    notif_row.add(&test_notif_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&notif_row, 0, SizerFlag::All, 2);
    let sounds_cb = add_check(
        &dialog,
        &sizer,
        "Play sounds when a refresh finishes",
        cfg.get_bool("sounds_enabled", true),
    );

    // Window / tray / backup / updates / advanced.
    add_heading(&dialog, &sizer, "Window and Tray");
    let close_tray_cb = add_check(
        &dialog,
        &sizer,
        "Close button hides to the tray",
        cfg.get_bool("close_to_tray", false),
    );
    let start_tray_cb = add_check(
        &dialog,
        &sizer,
        "Start in the system tray",
        cfg.get_bool("start_in_system_tray", false),
    );
    add_heading(&dialog, &sizer, "Backup");
    let auto_backup_cb = add_check(
        &dialog,
        &sizer,
        "Back up daily, automatically (keeps 7)",
        cfg.get_bool("auto_backup_enabled", false),
    );
    let auto_backup_dir_field = add_field(
        &dialog,
        &sizer,
        "Auto-backup folder (blank = data folder/backups)",
        cfg.get_str("auto_backup_dir", ""),
        false,
    );
    add_heading(&dialog, &sizer, "Updates");
    let auto_check_cb = add_check(
        &dialog,
        &sizer,
        "Check for updates on startup",
        cfg.get_bool("auto_check_updates", true),
    );
    let auto_install_cb = add_check(
        &dialog,
        &sizer,
        "Install updates automatically",
        cfg.get_bool("install_updates_automatically", false),
    );
    // Translation (six LLM providers).
    add_heading(&dialog, &sizer, "Translation");
    let translation_cb = add_check(
        &dialog,
        &sizer,
        "Translation enabled",
        cfg.get_bool("translation_enabled", false),
    );
    let tr_sel = blindrss_core::translate::PROVIDERS
        .iter()
        .position(|p| *p == cfg.get_str("translation_provider", "grok"))
        .unwrap_or(0);
    let tr_provider_choice = add_choice(
        &dialog,
        &sizer,
        "Translation provider",
        &blindrss_core::translate::PROVIDER_LABELS,
        tr_sel,
    );
    let tr_target_field = add_field(
        &dialog,
        &sizer,
        "Target language (code or name)",
        cfg.get_str("translation_target_language", "en"),
        false,
    );
    let mut tr_fields: Vec<(TextCtrl, TextCtrl)> = Vec::new();
    for (key, label) in blindrss_core::translate::PROVIDERS
        .iter()
        .zip(blindrss_core::translate::PROVIDER_LABELS.iter())
    {
        let key_field = add_field(
            &dialog,
            &sizer,
            &format!("{label} API key"),
            cfg.get_str(&format!("translation_{key}_api_key"), ""),
            true,
        );
        let model_field = add_field(
            &dialog,
            &sizer,
            &format!("{label} model (blank = defaults)"),
            cfg.get_str(&format!("translation_{key}_model"), ""),
            false,
        );
        tr_fields.push((key_field, model_field));
    }

    // Translation updates (OTA catalog channel).
    add_heading(&dialog, &sizer, "Translation Updates");
    let tu_enabled_cb = add_check(
        &dialog,
        &sizer,
        "Update translations automatically",
        cfg.get_bool(blindrss_core::translation_updates::CFG_ENABLED, true),
    );
    let tu_freq_values = ["ten_minutes", "daily", "weekly", "monthly"];
    let tu_freq_sel = tu_freq_values
        .iter()
        .position(|v| {
            *v == cfg.get_str(
                blindrss_core::translation_updates::CFG_FREQUENCY,
                blindrss_core::translation_updates::DEFAULT_FREQUENCY,
            )
        })
        .unwrap_or(1);
    let tu_freq_choice = add_choice(
        &dialog,
        &sizer,
        "Check for updates:",
        &["10 minutes", "Daily", "Weekly", "Monthly"],
        tu_freq_sel,
    );
    let tu_status = StaticText::builder(&dialog).with_label("").build();
    {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let check_btn = Button::builder(&dialog)
            .with_label(&crate::tr("Check for Updates"))
            .build();
        row.add(&check_btn, 0, SizerFlag::All, 4);
        row.add(&tu_status, 0, SizerFlag::All, 4);
        sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
        let (txc, rxc) = std::sync::mpsc::channel::<String>();
        let rxc = std::rc::Rc::new(std::cell::RefCell::new(rxc));
        let timer = Box::leak(Box::new(Timer::new(&dialog)));
        timer.start(200, false);
        {
            let rxc_tick = rxc.clone();
            timer.on_tick(move |_| {
                if let Ok(note) = rxc_tick.borrow().try_recv() {
                    tu_status.set_label(&note);
                }
            });
        }
        check_btn.on_click(move |_| {
            tu_status.set_label("Checking...");
            let txc = txc.clone();
            std::thread::spawn(move || {
                let cfg =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                let mut langs: Vec<String> = Vec::new();
                for root in crate::locale_roots() {
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
                // The UI language goes first: it is the one the
                // user can see.
                let ui = crate::ui_language();
                langs.retain(|l| l != &ui);
                langs.insert(0, ui);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0) as i64;
                let result = blindrss_core::translation_updates::check_and_update(
                    &crate::data_dir(),
                    &crate::locale_roots(),
                    &langs,
                    &blindrss_core::translation_updates::HttpFetch,
                    now,
                );
                let _ = cfg;
                let note = if result.changed() {
                    format!(
                        "Updated: {}. Applies on next launch.",
                        result.updated.join(", ")
                    )
                } else if let Some(e) = result.error {
                    format!("Check failed: {e}")
                } else {
                    "Translations are up to date.".to_string()
                };
                let _ = txc.send(note);
            });
        });
    }

    add_heading(&dialog, &sizer, "Advanced");
    let mut ua_items: Vec<&str> = vec!["Automatic (matches this computer)"];
    ua_items.extend(
        blindrss_core::user_agents::PRESETS
            .iter()
            .map(|(_, l, _)| *l),
    );
    ua_items.push("Custom (use the text below)");
    let ua_mode0 = cfg.get_str("user_agent_mode", "auto").to_string();
    let ua_sel = if ua_mode0 == "custom" {
        ua_items.len() - 1
    } else if ua_mode0 == "auto" || ua_mode0.is_empty() {
        0
    } else {
        blindrss_core::user_agents::PRESETS
            .iter()
            .position(|(k, _, _)| *k == ua_mode0)
            .map(|i| i + 1)
            .unwrap_or(0)
    };
    let ua_choice = add_choice(
        &dialog,
        &sizer,
        "Browser identity (User-Agent)",
        &ua_items,
        ua_sel,
    );
    let ua_custom_field = add_field(
        &dialog,
        &sizer,
        "Custom User-Agent",
        cfg.get_str("user_agent_custom", ""),
        false,
    );
    let cookies_file_field = add_field(
        &dialog,
        &sizer,
        "yt-dlp cookies file (optional)",
        cfg.get_str("ytdlp_cookies_file", ""),
        false,
    );
    let dep_prompt_cb = add_check(
        &dialog,
        &sizer,
        "Prompt about missing tools (yt-dlp, ffmpeg) on startup",
        cfg.get_bool("prompt_missing_dependencies_on_startup", true),
    );
    let captcha_cb = add_check(
        &dialog,
        &sizer,
        "Captcha solver enabled (last-resort fallback)",
        cfg.get_bool("captcha_solver_enabled", false),
    );
    let captcha_provider_field = add_field(
        &dialog,
        &sizer,
        "Captcha solver provider",
        cfg.get_str("captcha_solver_provider", ""),
        false,
    );
    let captcha_key_field = add_field(
        &dialog,
        &sizer,
        "Captcha solver API key",
        cfg.get_str("captcha_solver_api_key", ""),
        true,
    );
    let data_loc = StaticText::builder(&dialog)
        .with_label(&format!("Data folder: {}", crate::data_dir().display()))
        .build();
    sizer.add(&data_loc, 0, SizerFlag::All, 4);

    // Maintenance actions (moved off the main window, Brandon
    // 2026-10-30): each saves the fields, closes the dialog, and
    // reports its name back so the caller runs it on the main frame.
    let intent: std::rc::Rc<std::cell::RefCell<Option<&'static str>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let maint_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Maintenance"))
        .build();
    sizer.add(&maint_label, 0, SizerFlag::All, 4);
    let maint_row1 = BoxSizer::builder(Orientation::Horizontal).build();
    let maint_row2 = BoxSizer::builder(Orientation::Horizontal).build();
    let backup_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Backup Now"))
        .build();
    let restore_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Restore Backup..."))
        .build();
    let takeout_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Import YouTube Takeout..."))
        .build();
    let archive_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Rescan Podcast Archive"))
        .build();
    let sync_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Sync Providers Now"))
        .build();
    let prune_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Prune Old Articles"))
        .build();
    let update_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Check for Updates"))
        .build();
    let columns_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Article Columns..."))
        .build();
    maint_row1.add(&backup_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&restore_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&takeout_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&archive_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&sync_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&prune_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&update_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&columns_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&maint_row1, 0, SizerFlag::All, 2);
    sizer.add_sizer(&maint_row2, 0, SizerFlag::All, 2);

    let btn_row2 = BoxSizer::builder(Orientation::Horizontal).build();
    let connect_listener_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Connect Inoreader (browser sign-in)..."))
        .build();
    let test_miniflux_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Test Miniflux Connection"))
        .build();
    let youtube_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Connect YouTube Account..."))
        .build();
    btn_row2.add(&connect_listener_btn, 0, SizerFlag::All, 4);
    btn_row2.add(&test_miniflux_btn, 0, SizerFlag::All, 4);
    btn_row2.add(&youtube_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row2, 0, SizerFlag::All, 2);

    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let connect_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Connect Inoreader..."))
        .build();
    let save_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Save"))
        .build();
    let cancel_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    btn_row.add(&connect_btn, 0, SizerFlag::All, 4);
    btn_row.add(&save_btn, 0, SizerFlag::All, 4);
    btn_row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(620, 1180));

    let cfg_path_buf = config_path.to_path_buf();
    let cfg_path_connect = config_path.to_path_buf();
    // One save routine shared by Save and the maintenance buttons.
    let cfg_rc = std::rc::Rc::new(std::cell::RefCell::new(cfg));
    let startup_was = cfg_rc.borrow().get_bool("start_on_windows_login", false);
    let dialog_s = dialog;
    let save_all: std::rc::Rc<dyn Fn()> = {
        let cfg_s = cfg_rc.clone();
        let path_s = cfg_path_buf.clone();
        std::rc::Rc::new(move || {
            let mut cfg = cfg_s.borrow_mut();
            if let Some(sel) = active_choice.get_selection()
                && let Some(name) = blindrss_core::provider::PROVIDER_NAMES.get(sel as usize)
            {
                cfg.set_path("active_provider", serde_json::json!(name));
            }
            cfg.set_path(
                "providers.theoldreader.email",
                serde_json::json!(tor_email.get_value()),
            );
            cfg.set_path(
                "providers.theoldreader.password",
                serde_json::json!(tor_pass.get_value()),
            );
            cfg.set_path(
                "providers.bazqux.email",
                serde_json::json!(baz_email.get_value()),
            );
            cfg.set_path(
                "providers.bazqux.password",
                serde_json::json!(baz_pass.get_value()),
            );
            cfg.set_path(
                "providers.miniflux.url",
                serde_json::json!(mini_url.get_value()),
            );
            cfg.set_path(
                "providers.miniflux.api_key",
                serde_json::json!(mini_key.get_value()),
            );
            cfg.set_path(
                "providers.inoreader.app_id",
                serde_json::json!(ino_id.get_value()),
            );
            cfg.set_path(
                "providers.inoreader.app_key",
                serde_json::json!(ino_key.get_value()),
            );
            cfg.set_path("download_path", serde_json::json!(dl_path.get_value()));
            cfg.set_path(
                "global_show_hide_hotkey",
                serde_json::json!(hotkey_field.get_value().trim()),
            );
            cfg.set_path(
                "remember_last_feed",
                serde_json::json!(remember_cb.is_checked()),
            );
            cfg.set_path(
                "start_maximized",
                serde_json::json!(maximized_cb.is_checked()),
            );
            cfg.set_path(
                "category_tree_default_expanded",
                serde_json::json!(expand_cb.is_checked()),
            );
            cfg.set_path(
                "refresh_on_startup",
                serde_json::json!(refresh_startup_cb.is_checked()),
            );
            if let Ok(secs) = interval_field.get_value().trim().parse::<i64>() {
                cfg.set_path("refresh_interval", serde_json::json!(secs.max(0)));
            }
            let mut modes = blindrss_core::announce::Announcements::from_config(&cfg);
            for (ev, choice) in blindrss_core::announce::EVENTS
                .iter()
                .zip(announce_choices.iter())
            {
                let idx = choice.get_selection().unwrap_or(3) as usize;
                modes.set_mode(ev.id, blindrss_core::announce::MODE_ORDER[idx.min(3)]);
            }
            cfg.set_path("announcements", modes.to_json());
            // Batch 5 pages: refresh engine, reading, downloads,
            // player, notifications/sounds, tray, backup,
            // updates, advanced.
            let num = |f: &TextCtrl, key: &str, lo: i64, hi: i64| {
                if let Ok(v) = f.get_value().trim().parse::<i64>() {
                    Some((key.to_string(), v.clamp(lo, hi)))
                } else {
                    None
                }
            };
            for (key, v) in [
                num(&max_conc_field, "max_concurrent_refreshes", 1, 64),
                num(&timeout_field, "feed_timeout_seconds", 1, 300),
                num(&retries_field, "feed_retry_attempts", 1, 10),
                num(&per_host_field, "per_host_max_connections", 1, 64),
                num(&max_dl_field, "max_downloads", 1, 1000),
                num(&seek_back_field, "seek_back_ms", 0, 600_000),
                num(&seek_fwd_field, "seek_forward_ms", 0, 600_000),
                num(&vol_step_field, "volume_step", 1, 50),
                num(&silence_ms_field, "silence_skip_min_ms", 50, 60_000),
                num(
                    &notif_max_field,
                    "windows_notifications_max_per_refresh",
                    0,
                    1000,
                ),
            ]
            .into_iter()
            .flatten()
            {
                cfg.set_path(&key, serde_json::json!(v));
            }
            if let Ok(v) = speed_field.get_value().trim().parse::<f64>() {
                cfg.set_path("playback_speed", serde_json::json!(v.clamp(0.25, 4.0)));
            }
            if let Ok(v) = silence_db_field.get_value().trim().parse::<f64>() {
                cfg.set_path("silence_skip_threshold_db", serde_json::json!(v));
            }
            cfg.set_path(
                "ignore_feed_cache",
                serde_json::json!(ignore_cache_cb.is_checked()),
            );
            cfg.set_path(
                "search_mode",
                serde_json::json!(if search_mode_choice.get_selection() == Some(1) {
                    "title_only"
                } else {
                    "title_content"
                }),
            );
            let delete_value = match delete_choice.get_selection() {
                Some(1) => "purge".to_string(),
                Some(2) => format!("category:{}", delete_path_field.get_value().trim()),
                _ => "deleted".to_string(),
            };
            cfg.set_path("delete_behavior", serde_json::json!(delete_value));
            cfg.set_path(
                "confirm_article_delete",
                serde_json::json!(confirm_delete_cb.is_checked()),
            );
            cfg.set_path(
                "downloads_enabled",
                serde_json::json!(downloads_enabled_cb.is_checked()),
            );
            if let Some(sel) = format_choice.get_selection()
                && let Some(id) = blindrss_core::download::DOWNLOAD_FORMAT_IDS.get(sel as usize)
            {
                cfg.set_path("download_format", serde_json::json!(id));
            }
            if let Some(sel) = art_ret_choice.get_selection()
                && let Some((id, _)) = blindrss_core::retention::RETENTION_CHOICES.get(sel as usize)
            {
                cfg.set_path("article_retention", serde_json::json!(id));
            }
            if let Some(sel) = dl_ret_choice.get_selection()
                && let Some((id, _)) = blindrss_core::retention::RETENTION_CHOICES.get(sel as usize)
            {
                cfg.set_path("download_retention", serde_json::json!(id));
            }
            cfg.set_path(
                "show_player_on_play",
                serde_json::json!(show_player_cb.is_checked()),
            );
            cfg.set_path(
                "sponsorblock_enabled",
                serde_json::json!(sponsor_cb.is_checked()),
            );
            let cats: Vec<String> = sponsor_cats_field
                .get_value()
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            cfg.set_path("sponsorblock_categories", serde_json::json!(cats));
            cfg.set_path("skip_silence", serde_json::json!(silence_cb.is_checked()));
            cfg.set_path(
                "windows_notifications_enabled",
                serde_json::json!(notif_cb.is_checked()),
            );
            cfg.set_path(
                "windows_notifications_include_feed_name",
                serde_json::json!(notif_name_cb.is_checked()),
            );
            cfg.set_path(
                "windows_notifications_show_summary_when_capped",
                serde_json::json!(notif_summary_cb.is_checked()),
            );
            cfg.set_path("sounds_enabled", serde_json::json!(sounds_cb.is_checked()));
            cfg.set_path(
                "close_to_tray",
                serde_json::json!(close_tray_cb.is_checked()),
            );
            cfg.set_path(
                "start_in_system_tray",
                serde_json::json!(start_tray_cb.is_checked()),
            );
            cfg.set_path(
                "auto_backup_enabled",
                serde_json::json!(auto_backup_cb.is_checked()),
            );
            cfg.set_path(
                "auto_backup_dir",
                serde_json::json!(auto_backup_dir_field.get_value()),
            );
            cfg.set_path(
                "auto_check_updates",
                serde_json::json!(auto_check_cb.is_checked()),
            );
            cfg.set_path(
                "install_updates_automatically",
                serde_json::json!(auto_install_cb.is_checked()),
            );
            let ua_value = match ua_choice.get_selection() {
                Some(0) | None => "auto".to_string(),
                Some(sel) if (sel as usize) == blindrss_core::user_agents::PRESETS.len() + 1 => {
                    "custom".to_string()
                }
                Some(sel) => blindrss_core::user_agents::PRESETS
                    .get((sel - 1) as usize)
                    .map(|(k, _, _)| k.to_string())
                    .unwrap_or_else(|| "auto".to_string()),
            };
            cfg.set_path("user_agent_mode", serde_json::json!(ua_value));
            cfg.set_path(
                "user_agent_custom",
                serde_json::json!(ua_custom_field.get_value()),
            );
            cfg.set_path(
                "ytdlp_cookies_file",
                serde_json::json!(cookies_file_field.get_value()),
            );
            cfg.set_path(
                "prompt_missing_dependencies_on_startup",
                serde_json::json!(dep_prompt_cb.is_checked()),
            );
            cfg.set_path(
                "translation_enabled",
                serde_json::json!(translation_cb.is_checked()),
            );
            if let Some(sel) = tr_provider_choice.get_selection()
                && let Some(p) = blindrss_core::translate::PROVIDERS.get(sel as usize)
            {
                cfg.set_path("translation_provider", serde_json::json!(p));
            }
            cfg.set_path(
                "translation_target_language",
                serde_json::json!(tr_target_field.get_value()),
            );
            cfg.set_path(
                blindrss_core::translation_updates::CFG_ENABLED,
                serde_json::json!(tu_enabled_cb.is_checked()),
            );
            if let Some(sel) = tu_freq_choice.get_selection()
                && let Some(v) = tu_freq_values.get(sel as usize)
            {
                cfg.set_path(
                    blindrss_core::translation_updates::CFG_FREQUENCY,
                    serde_json::json!(v),
                );
            }
            for (key, (key_field, model_field)) in blindrss_core::translate::PROVIDERS
                .iter()
                .zip(tr_fields.iter())
            {
                cfg.set_path(
                    &format!("translation_{key}_api_key"),
                    serde_json::json!(key_field.get_value()),
                );
                cfg.set_path(
                    &format!("translation_{key}_model"),
                    serde_json::json!(model_field.get_value()),
                );
            }
            cfg.set_path(
                "captcha_solver_enabled",
                serde_json::json!(captcha_cb.is_checked()),
            );
            cfg.set_path(
                "captcha_solver_provider",
                serde_json::json!(captcha_provider_field.get_value()),
            );
            cfg.set_path(
                "captcha_solver_api_key",
                serde_json::json!(captcha_key_field.get_value()),
            );
            // Audit 4 additions: reading (image alt, article
            // opening), external tool paths, start at login.
            cfg.set_path(
                "show_image_alt",
                serde_json::json!(show_alt_cb.is_checked()),
            );
            let open_method = match open_method_choice.get_selection() {
                Some(1) => "custom",
                _ => "default",
            };
            cfg.set_path("article_open_method", serde_json::json!(open_method));
            cfg.set_path(
                "article_open_command",
                serde_json::json!(open_cmd_field.get_value()),
            );
            cfg.set_path(
                "custom_ffmpeg_path",
                serde_json::json!(ffmpeg_path_field.get_value()),
            );
            cfg.set_path(
                "custom_ffprobe_path",
                serde_json::json!(ffprobe_path_field.get_value()),
            );
            cfg.set_path(
                "custom_ytdlp_path",
                serde_json::json!(ytdlp_path_field.get_value()),
            );
            let login_new = start_login_cb.is_checked();
            cfg.set_path("start_on_windows_login", serde_json::json!(login_new));
            let _ = cfg.save(&path_s);
            // Apply the OS registration when the login setting
            // changed (Python applies it on settings save and
            // reverts the stored key when registration fails).
            if login_new != startup_was
                && crate::startup::supported()
                && let Err(e) = crate::startup::set_enabled(login_new)
            {
                cfg.set_path("start_on_windows_login", serde_json::json!(startup_was));
                let _ = cfg.save(&path_s);
                let md = MessageDialog::builder(
                    &dialog_s,
                    &format!("Could not update the startup setting: {e}"),
                    "Startup",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                .build();
                md.show_modal();
            }
        })
    };
    let frame_test = dialog;
    announce_test_btn.on_click(move |_| {
        let ok = blindrss_core::announce::announce_test("BlindRSS test announcement");
        let md = MessageDialog::builder(
            &frame_test,
            if ok {
                "Test announcement sent to your screen reader."
            } else {
                "No speech or Braille output path is available on this system."
            },
            "Announcements",
        )
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
        .build();
        md.show_modal();
    });
    {
        let frame_ex = dialog;
        let path_ex = config_path.to_path_buf();
        exclude_btn.on_click(move |_| {
            crate::dialogs::show_exclude_feeds(&frame_ex, &path_ex);
        });
    }
    {
        let frame_mf = dialog;
        test_miniflux_btn.on_click(move |_| {
            let (url, key) = (mini_url.get_value(), mini_key.get_value());
            let result = if url.trim().is_empty() || key.trim().is_empty() {
                Err("Enter the Miniflux URL and API key first".to_string())
            } else {
                blindrss_core::providers::MinifluxClient::new(url.trim(), key.trim())
                    .test_connection()
            };
            let (msg, style) = match &result {
                Ok(user) => (
                    format!("Miniflux connected as {user}."),
                    MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                ),
                Err(e) => (
                    format!("Miniflux test failed: {e}"),
                    MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
                ),
            };
            let md = MessageDialog::builder(&frame_mf, &msg, "Miniflux")
                .with_style(style)
                .build();
            md.show_modal();
        });
    }
    {
        let frame_yt = dialog;
        let path_yt = config_path.to_path_buf();
        youtube_btn.on_click(move |_| {
            crate::dialogs::show_youtube_signin(&frame_yt, &path_yt);
        });
    }
    {
        let frame_ls = dialog;
        let path_ls = config_path.to_path_buf();
        connect_listener_btn.on_click(move |_| {
            let app_id = ino_id.get_value();
            let app_key = ino_key.get_value();
            if app_id.is_empty() || app_key.is_empty() {
                let md = MessageDialog::builder(
                    &frame_ls,
                    "Enter the Inoreader app ID and app key first, then connect.",
                    "Inoreader",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                return;
            }
            inoreader_listener_connect(&frame_ls, &path_ls, app_id, app_key);
        });
    }
    test_notif_btn.on_click(move |_| test_notify());
    {
        let frame_oc = dialog;
        open_cmd_test_btn.on_click(move |_| {
            let template = open_cmd_field.get_value();
            let template = template.trim();
            if template.is_empty() {
                let md = MessageDialog::builder(
                    &frame_oc,
                    "Enter a command to test. Use %1 where the article URL should go.",
                    "Nothing to test",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                return;
            }
            let test_url = "https://example.com/";
            let (msg, title, style) =
                match blindrss_core::opencmd::build_open_command(template, test_url) {
                    Err(e) => (
                        format!("The command could not be parsed:\n\n{e}"),
                        "Invalid command",
                        MessageDialogStyle::OK | MessageDialogStyle::IconError,
                    ),
                    Ok(argv) => match std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .spawn()
                    {
                        Ok(_) => (
                            format!(
                                "Launched the test command with a sample URL:\n\n{}\n\nCheck that your browser opened https://example.com/ as expected.",
                                argv.join(" ")
                            ),
                            "Test launched",
                            MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                        ),
                        Err(e) => {
                            let err = if e.kind() == std::io::ErrorKind::NotFound {
                                format!("Command not found: {}", argv[0])
                            } else {
                                e.to_string()
                            };
                            (
                                format!("The command could not be run:\n\n{err}"),
                                "Test failed",
                                MessageDialogStyle::OK | MessageDialogStyle::IconError,
                            )
                        }
                    },
                };
            let md = MessageDialog::builder(&frame_oc, &msg, title)
                .with_style(style)
                .build();
            md.show_modal();
        });
    }
    let dialog_save = dialog;
    let save_for_btn = save_all.clone();
    save_btn.on_click(move |_| {
        save_for_btn();
        dialog_save.end_modal(5100);
    });
    // Maintenance buttons: save, record the intent, close.
    for (btn, name) in [
        (backup_btn, "backup"),
        (restore_btn, "restore"),
        (takeout_btn, "takeout"),
        (archive_btn, "archive"),
        (sync_btn, "sync"),
        (prune_btn, "prune"),
        (update_btn, "update"),
        (columns_btn, "columns"),
        (eq_btn, "equalizer"),
    ] {
        let save_m = save_all.clone();
        let intent_m = intent.clone();
        let dialog_m = dialog;
        btn.on_click(move |_| {
            save_m();
            *intent_m.borrow_mut() = Some(name);
            dialog_m.end_modal(5100);
        });
    }
    let dialog_cancel = dialog;
    cancel_btn.on_click(move |_| {
        dialog_cancel.end_modal(5101);
    });

    let dialog_connect = dialog;
    let frame_connect = *frame;
    connect_btn.on_click(move |_| {
        let app_id = ino_id.get_value();
        let app_key = ino_key.get_value();
        if app_id.is_empty() || app_key.is_empty() {
            let md = MessageDialog::builder(
                &frame_connect,
                "Enter the Inoreader app ID and app key first, then connect.",
                "Inoreader",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
            .build();
            md.show_modal();
            return;
        }
        let state = format!("blindrss{}", std::process::id());
        let url = blindrss_core::oauth::authorization_url(
            &app_id,
            blindrss_core::oauth::DEFAULT_REDIRECT,
            &state,
        );
        open_url(&url);
        let entry = TextEntryDialog::builder(
            &dialog_connect,
            "After approving in the browser, paste the full redirect URL here:",
            "Inoreader sign-in",
        )
        .build();
        if entry.show_modal() != 5100 {
            return;
        }
        let Some(pasted) = entry.get_value() else {
            return;
        };
        let (code, _state) = blindrss_core::oauth::parse_oauth_redirect(&pasted);
        let Some(code) = code else {
            let md = MessageDialog::builder(
                &frame_connect,
                "That URL has no authorization code in it.",
                "Inoreader",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
            md.show_modal();
            return;
        };
        match blindrss_core::oauth::exchange_code(
            &app_id,
            &app_key,
            &code,
            blindrss_core::oauth::DEFAULT_REDIRECT,
        ) {
            Ok(tokens) => {
                let mut cfg2 = Config::load(&cfg_path_connect).unwrap_or_default();
                if let Some(t) = tokens.get("access_token") {
                    cfg2.set_path("providers.inoreader.token", t.clone());
                }
                if let Some(t) = tokens.get("refresh_token") {
                    cfg2.set_path("providers.inoreader.refresh_token", t.clone());
                }
                if let Some(exp) = tokens.get("expires_in").and_then(|v| v.as_i64()) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    cfg2.set_path(
                        "providers.inoreader.token_expires_at",
                        serde_json::json!(now + exp),
                    );
                }
                cfg2.set_path("providers.inoreader.app_id", serde_json::json!(app_id));
                cfg2.set_path("providers.inoreader.app_key", serde_json::json!(app_key));
                let _ = cfg2.save(&cfg_path_connect);
                let md =
                    MessageDialog::builder(&frame_connect, "Inoreader connected.", "Inoreader")
                        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                        .build();
                md.show_modal();
            }
            Err(e) => {
                let md = MessageDialog::builder(
                    &frame_connect,
                    &format!("Inoreader sign-in failed: {e}"),
                    "Inoreader",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                .build();
                md.show_modal();
            }
        }
    });

    dialog.show_modal();
    dialog.destroy();
    *intent.borrow()
}
