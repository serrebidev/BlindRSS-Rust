//! Settings dialog: a Notebook of pages mirroring Python's
//! SettingsDialog (gui/dialogs.py), in Python's fixed page
//! order: General, Feeds & Articles, YouTube, Media Player,
//! Provider, Notifications, Translate, List Headers, Advanced,
//! CAPTCHA Solving. Never insert a page mid-notebook — tab
//! positions are screen-reader muscle memory.
//!
//! Writes into the shared Python-format config.json
//! (providers.theoldreader / bazqux / miniflux / inoreader,
//! download_path). Inoreader connect uses the paste-the-redirect
//! OAuth path (same as Python supports): open the auth page in the
//! browser, paste the final URL back, exchange the code.

use blindrss_core::config::Config;
use std::path::Path;
use wxdragon::prelude::*;
use wxdragon::sizers::base::Sizer;

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
    parent: &impl WxWidget,
    sizer: &Sizer,
    label: &str,
    value: &str,
    password: bool,
) -> TextCtrl {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(parent)
        .with_label(&crate::tr(label))
        .build();
    let style = if password {
        TextCtrlStyle::Password
    } else {
        TextCtrlStyle::default()
    };
    let ctrl = TextCtrl::builder(parent).with_style(style).build();
    ctrl.set_value(value);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    ctrl
}

/// A labeled checkbox row; returns the checkbox.
fn add_check(parent: &impl WxWidget, sizer: &Sizer, label: &str, value: bool) -> CheckBox {
    let cb = CheckBox::builder(parent)
        .with_label(&crate::tr(label))
        .build();
    cb.set_value(value);
    sizer.add(&cb, 0, SizerFlag::All, 2);
    cb
}

/// A labeled choice row; returns the choice control.
fn add_choice(
    parent: &impl WxWidget,
    sizer: &Sizer,
    label: &str,
    items: &[&str],
    selected: usize,
) -> Choice {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(parent)
        .with_label(&crate::tr(label))
        .build();
    let choice = Choice::builder(parent).build();
    // Display strings pass through tr() (Audit 8 S10); callers
    // always read the selection by index, never by comparing
    // the shown text, so stored keys stay stable.
    for item in items {
        choice.append(&crate::tr(item));
    }
    choice.set_selection(selected.min(items.len().saturating_sub(1)) as u32);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&choice, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    choice
}

/// A static explanatory paragraph (Python's page notes).
fn add_note(parent: &impl WxWidget, sizer: &Sizer, text: &str) {
    let note = StaticText::builder(parent).with_label(text).build();
    sizer.add(&note, 0, SizerFlag::All, 4);
}

/// One custom-sound row (Audit 8 S13; Python's `_add_sound_field`
/// plus the preview the audit asks for): path field, Browse...
/// (a WAV file dialog) and Play (the app's own sound player,
/// resolving the path exactly like the refresh sounds do:
/// beside the executable, then the data dir, then as given).
fn add_sound_row(
    dialog: &Dialog,
    parent: &impl WxWidget,
    sizer: &Sizer,
    label: &str,
    value: &str,
    default_rel: &'static str,
) -> TextCtrl {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(parent)
        .with_label(&crate::tr(label))
        .build();
    let ctrl = TextCtrl::builder(parent).build();
    ctrl.set_value(value);
    let field_name = label.trim_end_matches(':').to_string();
    crate::msaa_names::set_name(&ctrl, &field_name);
    let browse_btn = Button::builder(parent)
        .with_label(&crate::tr("Browse..."))
        .build();
    crate::msaa_names::set_name(
        &browse_btn,
        &format!("{} {}", crate::tr("Browse for"), field_name),
    );
    let play_btn = Button::builder(parent)
        .with_label(&crate::tr("Play"))
        .build();
    crate::msaa_names::set_name(&play_btn, &format!("{} {}", crate::tr("Play"), field_name));
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&browse_btn, 0, SizerFlag::All, 4);
    row.add(&play_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let dialog_b = *dialog;
    let label_owned = label.to_string();
    browse_btn.on_click(move |_| {
        let dlg = FileDialog::builder(&dialog_b)
            .with_message(&crate::tr(&label_owned))
            .with_wildcard("WAV files (*.wav)|*.wav|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open)
            .build();
        if dlg.show_modal() == 5100
            && let Some(path) = dlg.get_path()
        {
            ctrl.set_value(&path);
        }
    });
    play_btn.on_click(move |_| {
        let raw = ctrl.get_value();
        let raw = if raw.trim().is_empty() {
            default_rel
        } else {
            raw.trim()
        };
        let rel = std::path::Path::new(raw);
        let path = if rel.is_absolute() {
            rel.to_path_buf()
        } else if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
            && dir.join(rel).exists()
        {
            dir.join(rel)
        } else {
            let in_data = crate::data_dir().join(rel);
            if in_data.exists() {
                in_data
            } else {
                rel.to_path_buf()
            }
        };
        crate::play_sound_file(&path);
    });
    ctrl
}

/// One page of the settings notebook: a scrolled window (long
/// pages stay reachable, the same reason Python's Notifications
/// page is a ScrolledWindow) plus its vertical sizer.
struct SettingsPage {
    win: ScrolledWindow,
    sizer: BoxSizer,
}

impl SettingsPage {
    fn new(notebook: &Notebook) -> SettingsPage {
        let win = ScrolledWindow::builder(notebook).build();
        win.set_scroll_rate(0, 12);
        SettingsPage {
            win,
            sizer: BoxSizer::builder(Orientation::Vertical).build(),
        }
    }

    /// A labelled group on this page (Python's StaticBox groups:
    /// "Startup & Tray", "Downloads", "Sounds", ...). Controls are
    /// parented to the page and laid out by the returned sizer.
    fn group(&self, label: &str) -> StaticBoxSizer {
        let group =
            StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &self.win, label).build();
        self.sizer
            .add_sizer(&group, 0, SizerFlag::Expand | SizerFlag::All, 8);
        group
    }

    fn finish(&self) {
        self.win.set_sizer(self.sizer, true);
    }
}

/// Fire a test desktop notification from Settings (platform
/// commands; the real path uses the tray balloon on Windows).
fn test_notify() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        let script = "$n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(4000, 'BlindRSS', 'Test notification', [System.Windows.Forms.ToolTipIcon]::Info)";
        let mut cmd = std::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", script]);
        // CREATE_NO_WINDOW: never flash a console from a
        // windowed app (it steals screen-reader focus).
        cmd.creation_flags(0x0800_0000);
        let _ = cmd.spawn();
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

/// The Redirect URI field's effective value: the trimmed
/// field text, or the loopback default when blank (Python's
/// worker applies the same fallback before building the
/// authorization URL and exchanging the code).
fn trimmed_redirect(field: &TextCtrl) -> String {
    let value = field.get_value();
    let value = value.trim();
    if value.is_empty() {
        blindrss_core::oauth::DEFAULT_REDIRECT.to_string()
    } else {
        value.to_string()
    }
}

/// Inoreader sign-in through the local HTTPS listener
/// (Python's local-server OAuth variant): a self-signed
/// server on 127.0.0.1:18423 waits for the browser redirect
/// (the browser warns about the self-signed certificate —
/// the redirect target registered with Inoreader is this
/// https URL). The code is exchanged and the tokens saved,
/// exactly like the paste-the-URL flow. The configured
/// Redirect URI (Audit 8 S15) is used for the authorization
/// URL and the exchange in both flows; the listener itself
/// can only receive redirects aimed at the default loopback
/// address, so a custom URI belongs with the paste flow.
fn inoreader_listener_connect(
    frame: &Dialog,
    config_path: &Path,
    app_id: String,
    app_key: String,
    redirect_uri: String,
) {
    use std::io::{Read, Write};
    let state = format!("blindrss{}", std::process::id());
    let url = blindrss_core::oauth::authorization_url(&app_id, &redirect_uri, &state);
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
    dialog2.set_escape_id(5101);
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
                blindrss_core::oauth::exchange_code(&app_id, &app_key, &code, &redirect_uri)
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
                            serde_json::json!(blindrss_core::oauth::token_expires_at_from(
                                exp, now
                            )),
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
    dialog.set_escape_id(5101);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let notebook = Notebook::builder(&dialog).build();
    sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);

    // Pages in Python's fixed order (SettingsDialog in
    // gui/dialogs.py). Append new pages at the end only: tab
    // positions are screen-reader muscle memory.
    let general = SettingsPage::new(&notebook);
    let feeds = SettingsPage::new(&notebook);
    let youtube = SettingsPage::new(&notebook);
    let media = SettingsPage::new(&notebook);
    let provider = SettingsPage::new(&notebook);
    let notifications = SettingsPage::new(&notebook);
    let translate = SettingsPage::new(&notebook);
    let headers = SettingsPage::new(&notebook);
    let advanced = SettingsPage::new(&notebook);
    let captcha_page = SettingsPage::new(&notebook);
    notebook.add_page(&general.win, &crate::tr("General"), true, None);
    notebook.add_page(&feeds.win, &crate::tr("Feeds && Articles"), false, None);
    notebook.add_page(&youtube.win, &crate::tr("YouTube"), false, None);
    notebook.add_page(&media.win, &crate::tr("Media Player"), false, None);
    notebook.add_page(&provider.win, &crate::tr("Provider"), false, None);
    notebook.add_page(&notifications.win, &crate::tr("Notifications"), false, None);
    notebook.add_page(&translate.win, &crate::tr("Translate"), false, None);
    notebook.add_page(&headers.win, &crate::tr("List Headers"), false, None);
    notebook.add_page(&advanced.win, &crate::tr("Advanced"), false, None);
    notebook.add_page(
        &captcha_page.win,
        &crate::tr("CAPTCHA Solving"),
        false,
        None,
    );
    // F1 resolves through the current page (Audit 8 S06): the
    // English source titles, in add_page order, key core's
    // NOTEBOOK_PAGE_TOPICS exactly as Python's per-page
    // attributes do, whatever locale the tabs display in.
    crate::dialogs::bind_f1_notebook(
        frame,
        &dialog,
        "SettingsDialog",
        &notebook,
        &[
            "General",
            "Feeds && Articles",
            "YouTube",
            "Media Player",
            "Provider",
            "Notifications",
            "Translate",
            "List Headers",
            "Advanced",
            "CAPTCHA Solving",
        ],
    );

    // ---- General page (Python's General tab, in its order) ----
    let search_mode_choice = add_choice(
        &general.win,
        &general.sizer,
        "Search Matches:",
        &["Titles only", "Titles and article text"],
        if cfg.get_str("search_mode", "title_content") == "title_only" {
            0
        } else {
            1
        },
    );
    let confirm_delete_cb = add_check(
        &general.win,
        &general.sizer,
        "Confirm before deleting articles",
        cfg.get_bool("confirm_article_delete", true),
    );
    let delete_behavior0 = cfg.get_str("delete_behavior", "deleted").to_string();
    let delete_path0 = delete_behavior0
        .strip_prefix("category:")
        .unwrap_or("")
        .to_string();
    let delete_choice = add_choice(
        &general.win,
        &general.sizer,
        "When I delete an article:",
        &[
            "Move it to Deleted Articles (restorable)",
            "Remove it permanently",
            "Move it to a category",
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
        &general.win,
        &general.sizer,
        "Delete target category (full path)",
        &delete_path0,
        false,
    );
    let show_alt_cb = add_check(
        &general.win,
        &general.sizer,
        "Include image alt text in articles (announces images, can override per feed)",
        cfg.get_bool("show_image_alt", false),
    );
    let remember_cb = add_check(
        &general.win,
        &general.sizer,
        "Remember last selected feed/folder on startup",
        cfg.get_bool("remember_last_feed", false),
    );

    // Translation updates (OTA catalog channel): Python groups
    // these on General, directly under the language picker.
    let tu_group = general.group(&crate::tr("Translation Updates"));
    let tu_enabled_cb = add_check(
        &general.win,
        &tu_group,
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
        &general.win,
        &tu_group,
        "Check for updates:",
        &["10 minutes", "Daily", "Weekly", "Monthly"],
        tu_freq_sel,
    );
    let tu_status = StaticText::builder(&general.win).with_label("").build();
    {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let check_btn = Button::builder(&general.win)
            .with_label(&crate::tr("Check for Updates"))
            .build();
        row.add(&check_btn, 0, SizerFlag::All, 4);
        row.add(&tu_status, 0, SizerFlag::All, 4);
        tu_group.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
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
                // The manual check is Python's forced check
                // (gui/dialogs.py on_check_translations_now):
                // validators are bypassed so a stale or
                // stranded catalog state always re-fetches;
                // only the scheduled checks stay conditional
                // (Audit 8 O16).
                let result = blindrss_core::translation_updates::check_and_update(
                    &crate::data_dir(),
                    &crate::locale_roots(),
                    &langs,
                    &blindrss_core::translation_updates::HttpFetch,
                    now,
                    true,
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

    // Python's tree-state control is an expanded/collapsed choice;
    // the Rust checkbox for the same key predates this restructure
    // and keeps its form.
    let expand_cb = add_check(
        &general.win,
        &general.sizer,
        "Expand categories by default",
        cfg.get_bool("category_tree_default_expanded", false),
    );
    // Article opening (Python issue #31): default browser or a
    // custom command template with %1 for the URL.
    let open_method_choice = add_choice(
        &general.win,
        &general.sizer,
        "Article opening method:",
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
    let open_cmd_row = BoxSizer::builder(Orientation::Horizontal).build();
    let open_cmd_label = StaticText::builder(&general.win)
        .with_label("Custom command:")
        .build();
    let open_cmd_field = TextCtrl::builder(&general.win).build();
    open_cmd_field.set_value(cfg.get_str("article_open_command", ""));
    let open_cmd_test_btn = Button::builder(&general.win)
        .with_label(&crate::tr("Test"))
        .build();
    open_cmd_row.add(&open_cmd_label, 0, SizerFlag::All, 4);
    open_cmd_row.add(&open_cmd_field, 1, SizerFlag::Expand | SizerFlag::All, 4);
    open_cmd_row.add(&open_cmd_test_btn, 0, SizerFlag::All, 4);
    general
        .sizer
        .add_sizer(&open_cmd_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    add_note(
        &general.win,
        &general.sizer,
        &crate::tr("Use %1 for the article URL. Example: chrome --incognito %1"),
    );

    // Startup & Tray group (a group inside General in Python, not
    // its own tab).
    let startup_group = general.group(&crate::tr("Startup && Tray"));
    let close_tray_cb = add_check(
        &general.win,
        &startup_group,
        "Close to system tray",
        cfg.get_bool("close_to_tray", false),
    );
    let minimize_tray_cb = add_check(
        &general.win,
        &startup_group,
        "Minimize to system tray",
        cfg.get_bool("minimize_to_tray", true),
    );
    let start_tray_cb = add_check(
        &general.win,
        &startup_group,
        "Start BlindRSS in the system tray",
        cfg.get_bool("start_in_system_tray", false),
    );
    let hotkey_field = add_field(
        &general.win,
        &startup_group,
        "Show or hide BlindRSS from anywhere (hotkey, blank to turn off):",
        cfg.get_str("global_show_hide_hotkey", "Ctrl+Alt+B"),
        false,
    );
    let maximized_cb = add_check(
        &general.win,
        &startup_group,
        "Always start maximized",
        cfg.get_bool("start_maximized", false),
    );
    let auto_check_cb = add_check(
        &general.win,
        &startup_group,
        "Check for updates on startup",
        cfg.get_bool("auto_check_updates", true),
    );
    // Start at login (Python windows_integration): the checkbox
    // mirrors the config key; saving applies the OS registration
    // when the value changed. Disabled where unsupported.
    let start_login_cb = add_check(
        &general.win,
        &startup_group,
        crate::startup::setting_label(),
        cfg.get_bool("start_on_windows_login", false),
    );
    if !crate::startup::supported() {
        start_login_cb.enable(false);
    }

    // Backup group (inside General in Python).
    let backup_group = general.group(&crate::tr("Backup"));
    let auto_backup_cb = add_check(
        &general.win,
        &backup_group,
        "Back up BlindRSS data automatically once a day (keeps the last 7)",
        cfg.get_bool("auto_backup_enabled", false),
    );
    let auto_backup_dir_field = add_field(
        &general.win,
        &backup_group,
        "Automatic backup folder (blank = Backups beside your data):",
        cfg.get_str("auto_backup_dir", ""),
        false,
    );

    // Interface language (Audit 8 S09; Python gui/dialogs.py:1640).
    // Python places the picker above the Translation Updates
    // group; here it is appended at the end of the General page
    // per the append-only rule, so no existing control's tab
    // position moves. The stored `language` key is honored at
    // startup (crate::ui_language); the change applies on the
    // next launch — Python's "requires restart" — and no
    // hot-swap is invented.
    let (lang_codes, lang_items) = {
        let mut codes: Vec<String> = vec!["auto".to_string()];
        let mut items: Vec<String> = vec!["Automatic (system language)".to_string()];
        let mut roots = vec![blindrss_core::translation_updates::override_root(
            &crate::data_dir(),
        )];
        roots.extend(crate::locale_roots());
        let mut avail = blindrss_core::i18n::Catalog::available_languages(&roots);
        if !avail.iter().any(|c| c == "en") {
            avail.push("en".to_string());
        }
        let mut rest: Vec<(String, String)> = avail
            .into_iter()
            .filter(|c| c != "auto")
            .map(|code| {
                let display = match blindrss_core::translate::language_name(&code) {
                    Some(name) => format!("{} ({})", crate::tr(name), code),
                    None => code.clone(),
                };
                (display, code)
            })
            .collect();
        rest.sort_by(|a, b| a.0.cmp(&b.0));
        for (display, code) in rest {
            items.push(display);
            codes.push(code);
        }
        (codes, items)
    };
    let lang_sel = lang_codes
        .iter()
        .position(|c| c == cfg.get_str("language", "auto"))
        .unwrap_or(0);
    let lang_refs: Vec<&str> = lang_items.iter().map(|s| s.as_str()).collect();
    let lang_choice = add_choice(
        &general.win,
        &general.sizer,
        "Interface language (requires restart):",
        &lang_refs,
        lang_sel,
    );

    // Debug mode (Audit 8 S18; Python's General-panel checkbox):
    // opt-in rotating diagnostics log + the Windows console.
    // The runtime wiring lives in main.rs (diag_init).
    let debug_cb = add_check(
        &general.win,
        &general.sizer,
        "Debug mode (show console on startup)",
        cfg.get_bool("debug_mode", false),
    );

    // ---- Feeds & Articles page (Python's order) ----
    // Refresh scheduling (Python keys: refresh_interval is in
    // seconds, 300 default, 0 = off; Python picks from presets,
    // the Rust field takes seconds directly, as before).
    let interval_field = add_field(
        &feeds.win,
        &feeds.sizer,
        "Refresh Interval (seconds, 0 = off):",
        &cfg.get_i64("refresh_interval", 300).to_string(),
        false,
    );
    // Refresh engine (Audit 3 keys).
    let max_conc_field = add_field(
        &feeds.win,
        &feeds.sizer,
        "Max Concurrent Refreshes:",
        &cfg.get_i64("max_concurrent_refreshes", 16).to_string(),
        false,
    );
    let per_host_field = add_field(
        &feeds.win,
        &feeds.sizer,
        "Max Connections Per Host:",
        &cfg.get_i64("per_host_max_connections", 4).to_string(),
        false,
    );
    let timeout_field = add_field(
        &feeds.win,
        &feeds.sizer,
        "Feed Timeout (seconds):",
        &cfg.get_i64("feed_timeout_seconds", 15).to_string(),
        false,
    );
    let retries_field = add_field(
        &feeds.win,
        &feeds.sizer,
        "Feed Retry Attempts:",
        &cfg.get_i64("feed_retry_attempts", 1).to_string(),
        false,
    );
    let cache_full_text_cb = add_check(
        &feeds.win,
        &feeds.sizer,
        "Prefer web extraction over cached provider full text",
        cfg.get_bool("cache_full_text", false),
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
        &feeds.win,
        &feeds.sizer,
        "Article Retention:",
        &retention_refs,
        art_ret_sel,
    );
    let refresh_startup_cb = add_check(
        &feeds.win,
        &feeds.sizer,
        "Automatically refresh feeds upon start",
        cfg.get_bool("refresh_on_startup", true),
    );
    let workload = cfg.get_str(
        "automatic_feed_refresh_workload",
        if cfg.get_bool("ignore_feed_cache", false) {
            "always_full"
        } else {
            "startup_full"
        },
    );
    let refresh_workload = add_choice(
        &feeds.win,
        &feeds.sizer,
        "Local RSS automatic refresh workload:",
        &[
            "Use feed cache (fastest)",
            "Fully refresh feeds at startup",
            "Always fully refresh feeds",
        ],
        match workload {
            "cached" => 0,
            "always_full" => 2,
            _ => 1,
        },
    );
    add_note(
        &feeds.win,
        &feeds.sizer,
        &crate::tr(
            "Using the Local RSS feed cache lowers CPU and network use. Manual Refresh All always checks every feed.",
        ),
    );
    add_note(
        &feeds.win,
        &feeds.sizer,
        &crate::tr(
            "Article formatting: preserve structure from the original page as spoken text markers. Applies to newly loaded articles and full text.",
        ),
    );
    let structure_choices: Vec<(&str, CheckBox)> = [
        (
            "article_structure_tables",
            "Describe tables (\"Table with 3 rows...\" and one line per row)",
            true,
        ),
        (
            "article_structure_headings",
            "Announce headings (\"Heading level 2:\" before each heading)",
            false,
        ),
        (
            "article_structure_lists",
            "Mark list items with bullets and numbers",
            false,
        ),
        (
            "article_structure_quotes",
            "Mark quotations (\"Quote:\" before and \"End of quote.\" after)",
            false,
        ),
        (
            "article_structure_links",
            "Show links as \"text (address)\" and open the link at the cursor with Enter",
            false,
        ),
    ]
    .into_iter()
    .map(|(key, label, default)| {
        (
            key,
            add_check(&feeds.win, &feeds.sizer, label, cfg.get_bool(key, default)),
        )
    })
    .collect();
    let rich_view_cb = add_check(
        &feeds.win,
        &feeds.sizer,
        "Rich full-text view: show links, embedded videos, and tweets in a web view (instead of plain text). Falls back to plain text if unavailable.",
        cfg.get_bool("full_text_rich_view", false),
    );

    // ---- YouTube page (Python's order) ----
    add_note(
        &youtube.win,
        &youtube.sizer,
        &crate::tr(
            "Cookies are only needed for age-restricted, private, or members-only content. BlindRSS reads them automatically from your installed browsers (Firefox, Chrome, Edge, Brave, Opera, Vivaldi, Chromium and their beta/nightly variants). A cookies.txt export remains a manual fallback.",
        ),
    );
    let cookies_file_field = add_field(
        &youtube.win,
        &youtube.sizer,
        "yt-dlp cookies file path",
        cfg.get_str("ytdlp_cookies_file", ""),
        false,
    );
    let browser_cookie_import_cb = add_check(
        &youtube.win,
        &youtube.sizer,
        "Read cookies directly from installed browsers (Firefox, Chrome, Edge, Brave, Opera, Vivaldi, Chromium and their beta/nightly variants). Chrome/Edge/Brave need a one-time Windows permission prompt.",
        cfg.get_bool("installed_browser_cookie_import_enabled", false),
    );
    let dep_prompt_cb = add_check(
        &youtube.win,
        &youtube.sizer,
        "Ask to install missing media dependencies on startup",
        cfg.get_bool("prompt_missing_dependencies_on_startup", true),
    );
    let sponsor_cb = add_check(
        &youtube.win,
        &youtube.sizer,
        "Skip SponsorBlock segments in YouTube videos (crowdsourced)",
        cfg.get_bool("sponsorblock_enabled", true),
    );
    // Python picks the categories in a checklist; the Rust control
    // is the comma-separated field it has always been.
    let sponsor_cats_field = add_field(
        &youtube.win,
        &youtube.sizer,
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
    // YouTube account sign-in (its own dialog; Python reaches it
    // from the Tools menu, the settings entry point is Rust's).
    let youtube_btn = Button::builder(&youtube.win)
        .with_label(&crate::tr("Connect YouTube Account..."))
        .build();
    youtube.sizer.add(&youtube_btn, 0, SizerFlag::All, 2);
    // Audit 8 Task 22 (S12): Python's remaining YouTube
    // controls — appended after the existing ones, never
    // inserted mid-page (control order is screen-reader muscle
    // memory). Labels are Python's. The max-size value is a
    // text field, matching this dialog's other numeric fields,
    // where Python uses a spin control.
    let auto_import_cookies_cb = add_check(
        &youtube.win,
        &youtube.sizer,
        "Automatically import browser cookies.txt exports from Downloads",
        cfg.get_bool("auto_import_browser_cookies", true),
    );
    let play_via_download_cb = add_check(
        &youtube.win,
        &youtube.sizer,
        "Play YouTube by downloading first (most reliable; slower to start)",
        cfg.get_bool("youtube_play_via_download", false),
    );
    let play_cache_dir_field = add_field(
        &youtube.win,
        &youtube.sizer,
        "YouTube playback cache folder (blank = default, next to your data):",
        cfg.get_str("youtube_play_cache_dir", ""),
        false,
    );
    let play_cache_max_mb_field = add_field(
        &youtube.win,
        &youtube.sizer,
        "Max cache size (MB, 0 = unlimited):",
        &cfg.get_i64("youtube_play_cache_max_mb", 500).to_string(),
        false,
    );
    let clear_cache_btn = Button::builder(&youtube.win)
        .with_label(&crate::tr("Clear cache now"))
        .build();
    youtube.sizer.add(&clear_cache_btn, 0, SizerFlag::All, 2);

    // ---- Media Player page (Python's order) ----
    let mut soundcards = vec![("System Default".to_string(), String::new())];
    soundcards.extend(crate::media_backend::soundcards(&cfg));
    let saved_soundcard = cfg.get_str("preferred_soundcard", "").to_string();
    if !saved_soundcard.is_empty() && !soundcards.iter().any(|(_, id)| id == &saved_soundcard) {
        soundcards.push((
            format!("Saved device (currently unavailable): {saved_soundcard}"),
            saved_soundcard.to_string(),
        ));
    }
    let labels: Vec<&str> = soundcards.iter().map(|(label, _)| label.as_str()).collect();
    let soundcard_choice = add_choice(
        &media.win,
        &media.sizer,
        "Preferred Soundcard:",
        &labels,
        soundcards
            .iter()
            .position(|(_, id)| id == &saved_soundcard)
            .unwrap_or(0),
    );
    let silence_cb = add_check(
        &media.win,
        &media.sizer,
        "Skip Silence (Experimental)",
        cfg.get_bool("skip_silence", true),
    );
    let speed_field = add_field(
        &media.win,
        &media.sizer,
        "Default Playback Speed:",
        &cfg.raw()
            .get("playback_speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0)
            .to_string(),
        false,
    );
    let show_player_cb = add_check(
        &media.win,
        &media.sizer,
        "Show player window when starting playback",
        // Python's DEFAULT_CONFIG value is False and its loader
        // merges defaults into every config, so the effective
        // default is unchecked (Audit 8 M10).
        cfg.get_bool("show_player_on_play", false),
    );
    let resume_cb = add_check(
        &media.win,
        &media.sizer,
        "Resume playback from the saved position",
        cfg.get_bool("resume_playback", true),
    );
    let playback_fields: Vec<(&str, TextCtrl, i64, i64)> = [
        (
            "vlc_network_caching_ms",
            "Network Cache (ms):",
            1000,
            0,
            60_000,
        ),
        (
            "resume_save_interval_s",
            "Save playback position every (seconds)",
            15,
            1,
            3600,
        ),
        (
            "resume_back_ms",
            "Rewind on resume (milliseconds)",
            10_000,
            0,
            600_000,
        ),
        (
            "resume_min_ms",
            "Minimum saved position to resume (milliseconds)",
            0,
            0,
            3_600_000,
        ),
        (
            "resume_complete_threshold_ms",
            "Treat remaining playback as complete below (milliseconds)",
            60_000,
            0,
            3_600_000,
        ),
        (
            "vlc_local_proxy_network_caching_ms",
            "VLC local proxy network buffer (milliseconds)",
            1000,
            0,
            60_000,
        ),
        (
            "vlc_local_proxy_file_caching_ms",
            "VLC local proxy file buffer (milliseconds)",
            1000,
            0,
            60_000,
        ),
    ]
    .into_iter()
    .map(|(key, label, default, lo, hi)| {
        (
            key,
            add_field(
                &media.win,
                &media.sizer,
                label,
                &cfg.get_i64(key, default).to_string(),
                false,
            ),
            lo,
            hi,
        )
    })
    .collect();
    let seek_back_field = add_field(
        &media.win,
        &media.sizer,
        "Seek back (milliseconds)",
        &cfg.get_i64("seek_back_ms", 10_000).to_string(),
        false,
    );
    let seek_fwd_field = add_field(
        &media.win,
        &media.sizer,
        "Seek forward (milliseconds)",
        &cfg.get_i64("seek_forward_ms", 10_000).to_string(),
        false,
    );
    let vol_step_field = add_field(
        &media.win,
        &media.sizer,
        "Volume step (percent)",
        &cfg.get_i64("volume_step", 5).to_string(),
        false,
    );
    let silence_db_field = add_field(
        &media.win,
        &media.sizer,
        "Silence threshold (dB)",
        &cfg.raw()
            .get("silence_skip_threshold_db")
            .and_then(|v| v.as_f64())
            .unwrap_or(-38.0)
            .to_string(),
        false,
    );
    let silence_ms_field = add_field(
        &media.win,
        &media.sizer,
        "Minimum silence (milliseconds)",
        &cfg.get_i64("silence_skip_min_ms", 700).to_string(),
        false,
    );
    // Equalizer (its own dialog, opened via the intent mechanism
    // like Article Columns).
    let eq_btn = Button::builder(&media.win)
        .with_label(&crate::tr("Equalizer..."))
        .build();
    media.sizer.add(&eq_btn, 0, SizerFlag::All, 2);

    // Media tools group (Python's group on the Media Player page):
    // user-chosen executable paths take the highest detection
    // priority (blank = auto-detect).
    let tools_group = media.group(&crate::tr("Media tools (ffmpeg, ffprobe, yt-dlp)"));
    let ffmpeg_path_field = add_field(
        &media.win,
        &tools_group,
        "FFmpeg path:",
        cfg.get_str("custom_ffmpeg_path", ""),
        false,
    );
    let ffprobe_path_field = add_field(
        &media.win,
        &tools_group,
        "FFprobe path:",
        cfg.get_str("custom_ffprobe_path", ""),
        false,
    );
    let ytdlp_path_field = add_field(
        &media.win,
        &tools_group,
        "yt-dlp path:",
        cfg.get_str("custom_ytdlp_path", ""),
        false,
    );

    // Downloads group: Python builds the Downloads panel eagerly
    // and shows it inside the Media Player page under this group
    // (it is not a notebook page of its own).
    let downloads_group = media.group(&crate::tr("Downloads"));
    let downloads_enabled_cb = add_check(
        &media.win,
        &downloads_group,
        "Enable Downloads",
        cfg.get_bool("downloads_enabled", false),
    );
    let dl_path = add_field(
        &media.win,
        &downloads_group,
        "Download Path:",
        &get_path(&cfg, "download_path"),
        false,
    );
    let dl_ret_sel = blindrss_core::retention::RETENTION_CHOICES
        .iter()
        .position(|(id, _)| *id == cfg.get_str("download_retention", "unlimited"))
        .unwrap_or(11);
    let dl_ret_choice = add_choice(
        &media.win,
        &downloads_group,
        "Retention Policy:",
        &retention_refs,
        dl_ret_sel,
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
        &media.win,
        &downloads_group,
        "Default video download format:",
        &format_labels,
        format_sel,
    );
    let max_dl_field = add_field(
        &media.win,
        &downloads_group,
        "Max downloads kept in the queue",
        &cfg.get_i64("max_downloads", 32).to_string(),
        false,
    );

    // Sounds group (inside Media Player in Python).
    let sounds_group = media.group(&crate::tr("Sounds"));
    let sounds_cb = add_check(
        &media.win,
        &sounds_group,
        "Enable Sound Notifications",
        cfg.get_bool("sounds_enabled", true),
    );
    // Custom refresh sounds (Audit 8 S13): the runtime already
    // plays the configured paths; the fields default to the
    // bundled relative paths like Python's config defaults.
    let sound_complete_field = add_sound_row(
        &dialog,
        &media.win,
        &sounds_group,
        "Refresh Complete Sound:",
        cfg.get_str("sound_refresh_complete", "sounds/refresh_complete.wav"),
        "sounds/refresh_complete.wav",
    );
    let sound_error_field = add_sound_row(
        &dialog,
        &media.win,
        &sounds_group,
        "Refresh Error Sound:",
        cfg.get_str("sound_refresh_error", "sounds/refresh_error.wav"),
        "sounds/refresh_error.wav",
    );

    // Range-cache debug (Audit 8 S17, Python's "Verbose
    // range-cache proxy debug logs" on its media panel):
    // appended at the END of the Media Player page rather
    // than Python's mid-page spot, so no existing control's
    // tab position moves (append-only rule; see PARITY).
    let range_cache_debug_cb = add_check(
        &media.win,
        &media.sizer,
        "Verbose range-cache proxy debug logs",
        cfg.get_bool("range_cache_debug", false),
    );

    // ---- Provider page (Python's order: active provider, then
    // the per-provider panels in Python's panel order) ----
    // Active provider (Python's Provider page + factory.py):
    // the backend the whole UI reads/writes through.
    let active_sel = blindrss_core::provider::PROVIDER_NAMES
        .iter()
        .position(|n| *n == cfg.get_str("active_provider", "local"))
        .unwrap_or(0);
    let active_choice = add_choice(
        &provider.win,
        &provider.sizer,
        "Active Provider:",
        &blindrss_core::provider::PROVIDER_LABELS,
        active_sel,
    );
    let mini_url = add_field(
        &provider.win,
        &provider.sizer,
        "Miniflux URL:",
        &get_path(&cfg, "providers.miniflux.url"),
        false,
    );
    let mini_key = add_field(
        &provider.win,
        &provider.sizer,
        "Miniflux API Key:",
        &get_path(&cfg, "providers.miniflux.api_key"),
        true,
    );
    let tor_email = add_field(
        &provider.win,
        &provider.sizer,
        "The Old Reader Email:",
        &get_path(&cfg, "providers.theoldreader.email"),
        false,
    );
    let tor_pass = add_field(
        &provider.win,
        &provider.sizer,
        "The Old Reader Password:",
        &get_path(&cfg, "providers.theoldreader.password"),
        true,
    );
    let ino_id = add_field(
        &provider.win,
        &provider.sizer,
        "Inoreader App ID:",
        &get_path(&cfg, "providers.inoreader.app_id"),
        false,
    );
    let ino_key = add_field(
        &provider.win,
        &provider.sizer,
        "Inoreader App Key:",
        &get_path(&cfg, "providers.inoreader.app_key"),
        true,
    );
    let baz_email = add_field(
        &provider.win,
        &provider.sizer,
        "BazQux Email:",
        &get_path(&cfg, "providers.bazqux.email"),
        false,
    );
    let baz_pass = add_field(
        &provider.win,
        &provider.sizer,
        "BazQux Password:",
        &get_path(&cfg, "providers.bazqux.password"),
        true,
    );
    // Inoreader custom Redirect URI (Audit 8 S15; appended at
    // the end of the provider fields per the append-only
    // control rule). Python's dialog supplies this URI to
    // both OAuth flows; empty means the loopback default.
    let ino_redirect = add_field(
        &provider.win,
        &provider.sizer,
        "Inoreader Redirect URI:",
        &blindrss_core::oauth::redirect_uri_from_config(&cfg),
        false,
    );
    let provider_btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let connect_btn = Button::builder(&provider.win)
        .with_label(&crate::tr("Connect Inoreader..."))
        .build();
    let connect_listener_btn = Button::builder(&provider.win)
        .with_label(&crate::tr("Connect Inoreader (browser sign-in)..."))
        .build();
    let test_miniflux_btn = Button::builder(&provider.win)
        .with_label(&crate::tr("Test Miniflux Connection"))
        .build();
    provider_btn_row.add(&connect_btn, 0, SizerFlag::All, 4);
    provider_btn_row.add(&connect_listener_btn, 0, SizerFlag::All, 4);
    provider_btn_row.add(&test_miniflux_btn, 0, SizerFlag::All, 4);
    provider
        .sizer
        .add_sizer(&provider_btn_row, 0, SizerFlag::All, 2);

    // Source credentials (Audit 8 S14; Python's groups on its
    // provider page, gui/dialogs.py:3088,3107): masked fields,
    // stored under the same top-level keys Python uses and
    // consumed by the existing Groups.io and GitHub source
    // code (core threads source_key resolution). Groups are
    // appended at the end of the page (append-only rule).
    // A group is announced by a screen reader with the field,
    // which is why each credential gets its own group.
    let gio_group = provider.group(&crate::tr("Groups.io"));
    add_note(
        &provider.win,
        &gio_group,
        &crate::tr(
            "Groups.io API key (optional). Public groups and RSS work without a key; the key enables complete paginated topics and member-only archives. Create one at groups.io/settings/apikeys.",
        ),
    );
    let gio_key_field = add_field(
        &provider.win,
        &gio_group,
        "Groups.io API key:",
        cfg.get_str("groups_io_api_key", ""),
        true,
    );
    crate::msaa_names::set_name(&gio_key_field, &crate::tr("Groups.io API key"));
    add_note(
        &provider.win,
        &gio_group,
        &crate::tr(
            "Search subscriptions keep the search URL and refresh results in the background. Browser cookies are imported only from Groups.io domains.",
        ),
    );
    let gh_group = provider.group(&crate::tr("GitHub"));
    add_note(
        &provider.win,
        &gh_group,
        &crate::tr(
            "GitHub personal access token (optional). Full text of pull requests, issues and commits works without one, but GitHub allows only 60 page reads an hour per connection; a token raises that limit and reaches private repositories. Create one at github.com/settings/tokens.",
        ),
    );
    let gh_token_field = add_field(
        &provider.win,
        &gh_group,
        "GitHub personal access token:",
        cfg.get_str("github_token", ""),
        true,
    );
    crate::msaa_names::set_name(&gh_token_field, &crate::tr("GitHub personal access token"));

    // ---- Notifications page (Python's order, with the
    // Announcements group it absorbs) ----
    add_note(
        &notifications.win,
        &notifications.sizer,
        &crate::tr("Windows toast notifications for new articles. Disabled by default."),
    );
    let notif_cb = add_check(
        &notifications.win,
        &notifications.sizer,
        "Enable notifications for new articles",
        cfg.get_bool("windows_notifications_enabled", false),
    );
    let notif_name_cb = add_check(
        &notifications.win,
        &notifications.sizer,
        "Include feed name in notification text",
        cfg.get_bool("windows_notifications_include_feed_name", true),
    );
    let notif_max_field = add_field(
        &notifications.win,
        &notifications.sizer,
        "Max notifications per refresh (0 = no limit):",
        &cfg.get_i64("windows_notifications_max_per_refresh", 0)
            .to_string(),
        false,
    );
    let notif_summary_cb = add_check(
        &notifications.win,
        &notifications.sizer,
        "Show a summary notification when notification cap is reached",
        cfg.get_bool("windows_notifications_show_summary_when_capped", true),
    );
    let notif_row = BoxSizer::builder(Orientation::Horizontal).build();
    let test_notif_btn = Button::builder(&notifications.win)
        .with_label(&crate::tr("Test Notification"))
        .build();
    let exclude_btn = Button::builder(&notifications.win)
        .with_label(&crate::tr("Exclude Feeds..."))
        .build();
    notif_row.add(&test_notif_btn, 0, SizerFlag::All, 4);
    notif_row.add(&exclude_btn, 0, SizerFlag::All, 4);
    notifications
        .sizer
        .add_sizer(&notif_row, 0, SizerFlag::All, 2);

    // Announcements group (PARITY 7.4): per-event speech/Braille
    // mode, mirroring Python's announcements group on this page.
    let announce_group = notifications.group(&crate::tr("Announcements"));
    let current_modes = blindrss_core::announce::Announcements::from_config(&cfg);
    let mut announce_choices: Vec<Choice> = Vec::new();
    for ev in blindrss_core::announce::EVENTS.iter() {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let lbl = StaticText::builder(&notifications.win)
            .with_label(ev.label)
            .build();
        row.add(&lbl, 1, SizerFlag::All, 4);
        let choice = Choice::builder(&notifications.win).build();
        for label in blindrss_core::announce::MODE_LABELS {
            choice.append(label);
        }
        choice.set_selection(current_modes.mode_index(ev.id) as u32);
        row.add(&choice, 0, SizerFlag::All, 4);
        announce_group.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
        announce_choices.push(choice);
    }
    let announce_test_btn = Button::builder(&notifications.win)
        .with_label(&crate::tr("Test Announcement"))
        .build();
    announce_group.add(&announce_test_btn, 0, SizerFlag::All, 4);

    // ---- Translate page (six LLM providers, Python's order:
    // model field before API key for each provider) ----
    add_note(
        &translate.win,
        &translate.sizer,
        &crate::tr(
            "Configure automatic article translation. Your API key is stored locally in config.json.",
        ),
    );
    let translation_cb = add_check(
        &translate.win,
        &translate.sizer,
        "Enable automatic translation for article content",
        cfg.get_bool("translation_enabled", false),
    );
    let tr_sel = blindrss_core::translate::PROVIDERS
        .iter()
        .position(|p| *p == cfg.get_str("translation_provider", "grok"))
        .unwrap_or(0);
    let tr_provider_choice = add_choice(
        &translate.win,
        &translate.sizer,
        "Provider:",
        &blindrss_core::translate::PROVIDER_LABELS,
        tr_sel,
    );
    // Target language (Audit 8 S19; Python gui/dialogs.py:3622):
    // an editable combo of "Name (code)" choices sorted by
    // display text, showing the stored code's preset name.
    // Saving resolves the text back to a code; custom values
    // are preserved verbatim (core language_code_from_ui plus
    // the display-string map for translated names).
    let tr_lang_map: std::rc::Rc<Vec<(String, String)>> = {
        let mut map: Vec<(String, String)> = Vec::new();
        for (name, code) in blindrss_core::translate::LANGUAGE_PRESETS {
            map.push((format!("{} ({})", crate::tr(name), code), code.to_string()));
            map.push((crate::tr(name), code.to_string()));
            map.push((format!("{name} ({code})"), code.to_string()));
            map.push((name.to_string(), code.to_string()));
        }
        std::rc::Rc::new(map)
    };
    let target_row = BoxSizer::builder(Orientation::Horizontal).build();
    let target_label = StaticText::builder(&translate.win)
        .with_label(&crate::tr("Target language:"))
        .build();
    target_row.add(&target_label, 0, SizerFlag::All, 4);
    let tr_target_combo = ComboBox::builder(&translate.win).build();
    {
        let mut choices: Vec<String> = blindrss_core::translate::LANGUAGE_PRESETS
            .iter()
            .map(|(name, code)| format!("{} ({})", crate::tr(name), code))
            .collect();
        choices.sort();
        choices.dedup();
        for choice in &choices {
            tr_target_combo.append(choice);
        }
    }
    {
        let saved_lang = cfg.get_str("translation_target_language", "en");
        let initial = if saved_lang.trim().is_empty() {
            format!("{} (en)", crate::tr("English"))
        } else {
            let disp = blindrss_core::translate::language_display_value(saved_lang);
            if disp == saved_lang.trim() {
                disp
            } else {
                crate::tr(&disp)
            }
        };
        tr_target_combo.set_value(&initial);
    }
    crate::msaa_names::set_name(&tr_target_combo, &crate::tr("Target language"));
    target_row.add(&tr_target_combo, 1, SizerFlag::Expand | SizerFlag::All, 4);
    translate
        .sizer
        .add_sizer(&target_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    add_note(
        &translate.win,
        &translate.sizer,
        &crate::tr("Choose a language or type a code (e.g. en, es, fr, pt-BR)."),
    );
    // Per-provider model + API key rows (Python's order: model
    // before key). Models are editable combos seeded with the
    // provider's candidate list (Audit 8 S19); a stored custom
    // model stays the field text even when it is not a
    // candidate, and is saved back verbatim.
    let mut tr_fields: Vec<(TextCtrl, ComboBox)> = Vec::new();
    let mut or_model_combo: Option<ComboBox> = None;
    let mut or_key_field: Option<TextCtrl> = None;
    let mut or_load_btn: Option<Button> = None;
    let mut or_status: Option<StaticText> = None;
    for (key, label) in blindrss_core::translate::PROVIDERS
        .iter()
        .zip(blindrss_core::translate::PROVIDER_LABELS.iter())
    {
        let model_row = BoxSizer::builder(Orientation::Horizontal).build();
        let model_label = StaticText::builder(&translate.win)
            .with_label(&crate::tr(&format!("{label} model (optional):")))
            .build();
        model_row.add(&model_label, 0, SizerFlag::All, 4);
        let model_combo = ComboBox::builder(&translate.win).build();
        for model in blindrss_core::translate::default_models(key) {
            model_combo.append(model);
        }
        model_combo.set_value(cfg.get_str(&format!("translation_{key}_model"), ""));
        crate::msaa_names::set_name(&model_combo, &crate::tr(&format!("{label} model")));
        model_row.add(&model_combo, 1, SizerFlag::Expand | SizerFlag::All, 4);
        translate
            .sizer
            .add_sizer(&model_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
        let key_field = add_field(
            &translate.win,
            &translate.sizer,
            &format!("{label} API key:"),
            cfg.get_str(&format!("translation_{key}_api_key"), ""),
            true,
        );
        if *key == "openrouter" {
            or_model_combo = Some(model_combo);
            or_key_field = Some(key_field);
            // OpenRouter model discovery (Python's tools row):
            // loads every model id the service offers into the
            // combo above, failure-safe.
            let tools_row = BoxSizer::builder(Orientation::Horizontal).build();
            let load_btn = Button::builder(&translate.win)
                .with_label(&crate::tr("Load OpenRouter Models"))
                .build();
            let status = StaticText::builder(&translate.win)
                .with_label(&crate::tr("Loads all available model IDs from OpenRouter."))
                .build();
            tools_row.add(&load_btn, 0, SizerFlag::All, 4);
            tools_row.add(&status, 0, SizerFlag::All, 4);
            translate
                .sizer
                .add_sizer(&tools_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
            or_load_btn = Some(load_btn);
            or_status = Some(status);
        }
        tr_fields.push((key_field, model_combo));
    }
    // The OpenRouter load runs on a worker; a dialog timer
    // drains the result (the Translation Updates pattern).
    if let (Some(load_btn), Some(status), Some(model_combo), Some(key_field)) =
        (or_load_btn, or_status, or_model_combo, or_key_field)
    {
        let (or_tx, or_rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
        let or_rx = std::rc::Rc::new(std::cell::RefCell::new(or_rx));
        let or_loading = std::rc::Rc::new(std::cell::RefCell::new(false));
        let or_timer = Box::leak(Box::new(Timer::new(&dialog)));
        or_timer.start(250, false);
        {
            let or_rx_tick = or_rx.clone();
            let or_loading_tick = or_loading.clone();
            or_timer.on_tick(move |_| {
                let Ok(result) = or_rx_tick.borrow().try_recv() else {
                    return;
                };
                *or_loading_tick.borrow_mut() = false;
                load_btn.enable(true);
                match result {
                    Ok(models) => {
                        let current = model_combo.get_value();
                        let choices = blindrss_core::translate::openrouter_model_choices(&models);
                        model_combo.clear();
                        for choice in &choices {
                            model_combo.append(choice);
                        }
                        if !current.trim().is_empty() {
                            model_combo.set_value(&current);
                        }
                        status.set_label(&if models.len() == 1 {
                            crate::tr("Loaded 1 OpenRouter model.")
                        } else {
                            format!(
                                "{} {} {}",
                                crate::tr("Loaded"),
                                models.len(),
                                crate::tr("OpenRouter models.")
                            )
                        });
                    }
                    Err(error) => {
                        status.set_label(&format!(
                            "{}: {}",
                            crate::tr("OpenRouter model load failed"),
                            error
                        ));
                    }
                }
            });
        }
        load_btn.on_click(move |_| {
            if *or_loading.borrow() {
                return;
            }
            *or_loading.borrow_mut() = true;
            load_btn.enable(false);
            status.set_label(&crate::tr("Loading OpenRouter models..."));
            let key = key_field.get_value();
            let or_tx = or_tx.clone();
            std::thread::spawn(move || {
                let _ = or_tx.send(blindrss_core::translate::list_openrouter_models(&key, 25));
            });
        });
    }

    // ---- List Headers page ----
    // Python's page IS the ColumnLayoutPanel. The Rust column
    // editor is the separate Article Columns dialog
    // (dialogs::show_article_columns), which loads the layout
    // from config and saves it on its own OK; embedding it here
    // would duplicate that editor inside this file. The page
    // therefore opens it through the existing "columns" intent,
    // exactly as the old Article Columns... button did.
    add_note(
        &headers.win,
        &headers.sizer,
        &crate::tr(
            "Choose which columns the article list shows, and in what order. The editor opens as its own dialog and saves when you choose OK there.",
        ),
    );
    let columns_btn = Button::builder(&headers.win)
        .with_label(&crate::tr("Article Columns..."))
        .build();
    headers.sizer.add(&columns_btn, 0, SizerFlag::All, 2);

    // ---- Advanced page (Python's groups, in its order) ----
    let storage_group = advanced.group(&crate::tr("Data Storage Location"));
    let data_loc = StaticText::builder(&advanced.win)
        .with_label(&format!("Data folder: {}", crate::data_dir().display()))
        .build();
    storage_group.add(&data_loc, 0, SizerFlag::All, 4);
    // Storage location picker (Python's Advanced page, Audit 8
    // S11): the current location is the one the running config
    // actually resolved to. Saving a different choice moves
    // config.json (core change_data_location); the database
    // migrates on the next startup, as in Python.
    let storage_is_user = config_path.parent() == Some(crate::user_data_dir().as_path());
    let storage_choice = add_choice(
        &advanced.win,
        &storage_group,
        "Storage Location:",
        &["User Data Folder", "App Install Folder"],
        if storage_is_user { 0 } else { 1 },
    );
    let storage_paths = StaticText::builder(&advanced.win)
        .with_label(&format!(
            "User Data Folder path: {}\nApp Install Folder path: {}",
            crate::user_data_dir().join("config.json").display(),
            crate::app_dir().join("config.json").display()
        ))
        .build();
    storage_group.add(&storage_paths, 0, SizerFlag::All, 4);
    let updates_group = advanced.group(&crate::tr("Updates"));
    let auto_install_cb = add_check(
        &advanced.win,
        &updates_group,
        "Automatically install updates without confirmation",
        cfg.get_bool("install_updates_automatically", false),
    );
    // Browser identity (core user_agents): the User-Agent the
    // app reports when fetching feeds and articles.
    let ua_group = advanced.group(&crate::tr("Browser Identification"));
    let mut ua_options: Vec<(String, String)> =
        vec![("auto".into(), "Automatic (matches this computer)".into())];
    ua_options.extend(blindrss_core::user_agents::installed_choices());
    ua_options.extend(
        blindrss_core::user_agents::PRESETS
            .iter()
            .map(|(key, label, _)| (key.to_string(), label.to_string())),
    );
    ua_options.push(("custom".into(), "Custom (use the text below)".into()));
    let ua_mode0 = cfg.get_str("user_agent_mode", "auto").to_string();
    if ua_mode0.starts_with("installed:") && !ua_options.iter().any(|(key, _)| key == &ua_mode0) {
        ua_options.insert(
            1,
            (
                ua_mode0.clone(),
                format!(
                    "{} (unavailable)",
                    ua_mode0.trim_start_matches("installed:")
                ),
            ),
        );
    }
    let ua_options = std::rc::Rc::new(ua_options);
    let ua_sel = ua_options
        .iter()
        .position(|(key, _)| key == &ua_mode0)
        .unwrap_or(0);
    let ua_items: Vec<&str> = ua_options.iter().map(|(_, label)| label.as_str()).collect();
    let ua_choice = add_choice(&advanced.win, &ua_group, "Identify as:", &ua_items, ua_sel);
    let ua_custom_field = add_field(
        &advanced.win,
        &ua_group,
        "Custom User-Agent:",
        cfg.get_str("user_agent_custom", ""),
        false,
    );
    // Resolved User-Agent preview (Audit 8 S16; Python's
    // "Current User-Agent" readout, gui/dialogs.py:4116): the
    // exact string the selected identity resolves to, refreshed
    // as the choice or the custom text changes, so the choice
    // is verifiable without leaving the dialog. The custom
    // field is enabled only in Custom mode, as in Python.
    let ua_preview = StaticText::builder(&advanced.win).with_label("").build();
    crate::msaa_names::set_name(&ua_preview, &crate::tr("Current User-Agent"));
    ua_group.add(&ua_preview, 0, SizerFlag::All, 4);
    let update_ua_preview: std::rc::Rc<dyn Fn()> = {
        let ua_options = ua_options.clone();
        std::rc::Rc::new(move || {
            let sel = ua_choice.get_selection().unwrap_or(0) as usize;
            let mode = ua_options
                .get(sel)
                .map(|(key, _)| key.as_str())
                .unwrap_or("auto");
            let is_custom = mode == "custom";
            ua_custom_field.enable(is_custom);
            let effective = if is_custom {
                let text = ua_custom_field.get_value();
                let text = text.trim();
                if text.is_empty() {
                    String::new()
                } else {
                    text.to_string()
                }
            } else {
                blindrss_core::user_agents::resolve(mode, "")
            };
            ua_preview.set_label(&format!(
                "{}: {}",
                crate::tr("Current User-Agent"),
                if effective.is_empty() {
                    crate::tr("(not set)")
                } else {
                    effective
                }
            ));
        })
    };
    update_ua_preview();
    {
        let update = update_ua_preview.clone();
        ua_choice.on_selection_changed(move |_| update());
    }
    {
        let update = update_ua_preview.clone();
        ua_custom_field.on_text_updated(move |_| update());
    }

    // Video Search content opt-in (Audit 8 S16; Python's group,
    // gui/dialogs.py:4126): adult sites stay out of the Video
    // Search site list until this is explicitly enabled — the
    // registry gate (videosearch searchable_sites) reads the
    // key, and even when on, an adult site must still be
    // selected explicitly to be searched.
    let adult_group = advanced.group(&crate::tr("Video Search"));
    let adult_cb = add_check(
        &advanced.win,
        &adult_group,
        "Enable adult sites in Video Search",
        cfg.get_bool("enable_adult_search", false),
    );
    add_note(
        &advanced.win,
        &adult_group,
        &crate::tr(
            "When off, adult sites never appear in the Video Search site list.\nWhen on, adult sites are added and must still be selected explicitly to search them.",
        ),
    );

    // ---- CAPTCHA Solving page (Python's order) ----
    add_note(
        &captcha_page.win,
        &captcha_page.sizer,
        &crate::tr(
            "Optional paid CAPTCHA-solving service for sites whose Cloudflare challenge defeats every automated browser attempt. BlindRSS then sends only the challenge parameters to the service and injects the solved token. The service sees which blocked URL is being solved. Your API key is stored locally in config.json.",
        ),
    );
    let captcha_cb = add_check(
        &captcha_page.win,
        &captcha_page.sizer,
        "Enable CAPTCHA solving service (last resort, per-solve fees apply)",
        cfg.get_bool("captcha_solver_enabled", false),
    );
    // Python picks the service in a choice of 2captcha /
    // SolveCaptcha; the Rust control is the free-text field it
    // has always been.
    let captcha_provider_field = add_field(
        &captcha_page.win,
        &captcha_page.sizer,
        "Service:",
        cfg.get_str("captcha_solver_provider", ""),
        false,
    );
    let captcha_key_field = add_field(
        &captcha_page.win,
        &captcha_page.sizer,
        "API key:",
        cfg.get_str("captcha_solver_api_key", ""),
        true,
    );

    for page in [
        &general,
        &feeds,
        &youtube,
        &media,
        &provider,
        &notifications,
        &translate,
        &headers,
        &advanced,
        &captcha_page,
    ] {
        page.finish();
    }

    // Maintenance actions (moved off the main window, Brandon
    // 2026-10-30): dialog-level, below the notebook like Python's
    // OK/Cancel row. Each saves the fields, closes the dialog, and
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
    maint_row1.add(&backup_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&restore_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&takeout_btn, 0, SizerFlag::All, 4);
    maint_row1.add(&archive_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&sync_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&prune_btn, 0, SizerFlag::All, 4);
    maint_row2.add(&update_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&maint_row1, 0, SizerFlag::All, 2);
    sizer.add_sizer(&maint_row2, 0, SizerFlag::All, 2);

    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let save_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Save"))
        .build();
    let cancel_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    btn_row.add(&save_btn, 0, SizerFlag::All, 4);
    btn_row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(660, 780));

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
            // Storage relocation first (Audit 8 S11): Python
            // applies a data_location change before the other
            // writes so they land in the new file. The database
            // migrates on the next startup. On failure Python
            // warns and the remaining settings still save to the
            // old file.
            let chosen_location = if storage_choice.get_selection() == Some(0) {
                "user_data"
            } else {
                "app_folder"
            };
            let current_location = if path_s.parent() == Some(crate::user_data_dir().as_path()) {
                "user_data"
            } else {
                "app_folder"
            };
            let mut save_path = path_s.clone();
            if chosen_location != current_location {
                match cfg.change_data_location(
                    &path_s,
                    chosen_location,
                    &crate::user_data_dir().join("config.json"),
                    &crate::app_dir().join("config.json"),
                    crate::windows_installed_build(),
                ) {
                    Ok(new_path) => {
                        save_path = new_path;
                        let md = MessageDialog::builder(
                            &dialog_s,
                            "Config has been moved. The feed database will be migrated to the new location the next time BlindRSS starts.",
                            "Storage Location Changed",
                        )
                        .with_style(
                            MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                        )
                        .build();
                        md.show_modal();
                    }
                    Err(e) => {
                        let md = MessageDialog::builder(&dialog_s, &e, "Storage Location")
                            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                            .build();
                        md.show_modal();
                    }
                }
            }
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
            cfg.set_path(
                "providers.inoreader.redirect_uri",
                serde_json::json!(ino_redirect.get_value()),
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
            // Refresh engine, reading, downloads, player,
            // notifications/sounds, tray, backup, updates, advanced.
            let num = |f: &TextCtrl, key: &str, lo: i64, hi: i64| {
                if let Ok(v) = f.get_value().trim().parse::<i64>() {
                    Some((key.to_string(), v.clamp(lo, hi)))
                } else {
                    None
                }
            };
            for (key, field, lo, hi) in &playback_fields {
                if let Some((key, value)) = num(field, key, *lo, *hi) {
                    cfg.set_path(&key, serde_json::json!(value));
                }
            }
            cfg.set_path("resume_playback", serde_json::json!(resume_cb.is_checked()));
            cfg.set_path(
                "preferred_soundcard",
                serde_json::json!(
                    soundcard_choice
                        .get_selection()
                        .and_then(|i| soundcards.get(i as usize))
                        .map(|(_, id)| id.as_str())
                        .unwrap_or(&saved_soundcard)
                ),
            );
            for (key, control) in &structure_choices {
                cfg.set_path(key, serde_json::json!(control.is_checked()));
            }
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
                serde_json::json!(refresh_workload.get_selection() == Some(2)),
            );
            cfg.set_path(
                "automatic_feed_refresh_workload",
                serde_json::json!(match refresh_workload.get_selection() {
                    Some(0) => "cached",
                    Some(2) => "always_full",
                    _ => "startup_full",
                }),
            );
            cfg.set_path(
                "search_mode",
                serde_json::json!(if search_mode_choice.get_selection() == Some(0) {
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
                "range_cache_debug",
                serde_json::json!(range_cache_debug_cb.is_checked()),
            );
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
            // Custom refresh sounds (Audit 8 S13): stored as
            // typed, like Python (no trim).
            cfg.set_path(
                "sound_refresh_complete",
                serde_json::json!(sound_complete_field.get_value()),
            );
            cfg.set_path(
                "sound_refresh_error",
                serde_json::json!(sound_error_field.get_value()),
            );
            // Interface language (Audit 8 S09): the picker index
            // maps to its code ("auto" included).
            if let Some(sel) = lang_choice.get_selection()
                && let Some(code) = lang_codes.get(sel as usize)
            {
                cfg.set_path("language", serde_json::json!(code));
            }
            // Debug mode (Audit 8 S18).
            cfg.set_path("debug_mode", serde_json::json!(debug_cb.is_checked()));
            // Source credentials (Audit 8 S14): stored trimmed,
            // like Python; secrets, never logged (the
            // diagnostics redactor covers these keys).
            cfg.set_path(
                "groups_io_api_key",
                serde_json::json!(gio_key_field.get_value().trim()),
            );
            cfg.set_path(
                "github_token",
                serde_json::json!(gh_token_field.get_value().trim()),
            );
            // Adult-site opt-in (Audit 8 S16).
            cfg.set_path(
                "enable_adult_search",
                serde_json::json!(adult_cb.is_checked()),
            );
            cfg.set_path(
                "close_to_tray",
                serde_json::json!(close_tray_cb.is_checked()),
            );
            cfg.set_path(
                "minimize_to_tray",
                serde_json::json!(minimize_tray_cb.is_checked()),
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
            let ua_value = ua_choice
                .get_selection()
                .and_then(|sel| ua_options.get(sel as usize))
                .map(|(key, _)| key.as_str())
                .unwrap_or("auto");
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
            // The combo text resolves back to a code through
            // the display map first (translated names
            // included), then core's parser (Audit 8 S19);
            // custom values stay verbatim.
            let tr_lang_raw = tr_target_combo.get_value();
            let tr_lang_code = tr_lang_map
                .iter()
                .find(|(display, _)| display == tr_lang_raw.trim())
                .map(|(_, code)| code.clone())
                .unwrap_or_else(|| blindrss_core::translate::language_code_from_ui(&tr_lang_raw));
            cfg.set_path(
                "translation_target_language",
                serde_json::json!(tr_lang_code),
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
            // Reading (image alt, article opening), external tool
            // paths, start at login.
            cfg.set_path(
                "show_image_alt",
                serde_json::json!(show_alt_cb.is_checked()),
            );
            cfg.set_path(
                "full_text_rich_view",
                serde_json::json!(rich_view_cb.is_checked()),
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
            cfg.set_path(
                "installed_browser_cookie_import_enabled",
                serde_json::json!(browser_cookie_import_cb.is_checked()),
            );
            cfg.set_path(
                "auto_import_browser_cookies",
                serde_json::json!(auto_import_cookies_cb.is_checked()),
            );
            cfg.set_path(
                "youtube_play_via_download",
                serde_json::json!(play_via_download_cb.is_checked()),
            );
            cfg.set_path(
                "youtube_play_cache_dir",
                serde_json::json!(play_cache_dir_field.get_value().trim()),
            );
            cfg.set_path(
                "youtube_play_cache_max_mb",
                serde_json::json!(
                    play_cache_max_mb_field
                        .get_value()
                        .trim()
                        .parse::<i64>()
                        .unwrap_or(500)
                        .clamp(0, 1_000_000)
                ),
            );
            cfg.set_path(
                "cache_full_text",
                serde_json::json!(cache_full_text_cb.is_checked()),
            );
            let _ = cfg.save(&save_path);
            // Apply the OS registration when the login setting
            // changed (Python applies it on settings save and
            // reverts the stored key when registration fails).
            if login_new != startup_was
                && crate::startup::supported()
                && let Err(e) = crate::startup::set_enabled(login_new)
            {
                cfg.set_path("start_on_windows_login", serde_json::json!(startup_was));
                let _ = cfg.save(&save_path);
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
        // Clear the playback cache (S12; Python
        // _on_clear_play_cache): scoped to the folder in the
        // (possibly unsaved) field, or the resolved default —
        // counted first, confirmed, then cleared and reported.
        let frame_cc = dialog;
        let field_cc = play_cache_dir_field;
        clear_cache_btn.on_click(move |_| {
            let loc = field_cc.get_value();
            let loc = loc.trim();
            let dir = if loc.is_empty() {
                blindrss_core::play_cache::cache_dir(None, &crate::data_dir())
            } else {
                std::path::PathBuf::from(loc)
            };
            let count = blindrss_core::play_cache::file_count(&dir);
            if count == 0 {
                let md = MessageDialog::builder(
                    &frame_cc,
                    &format!("The playback cache is already empty.\n\n{}", dir.display()),
                    "Playback cache",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                return;
            }
            let size = blindrss_core::play_cache::size_bytes(&dir);
            let confirm = MessageDialog::builder(
                &frame_cc,
                &format!(
                    "Delete {count} cached file{} ({})?\n\n{}",
                    if count == 1 { "" } else { "s" },
                    blindrss_core::play_cache::human_size(size),
                    dir.display()
                ),
                "Clear playback cache",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            if confirm.show_modal() != ID_YES {
                return;
            }
            let (removed, freed) = blindrss_core::play_cache::clear_cache(&dir);
            let md = MessageDialog::builder(
                &frame_cc,
                &format!(
                    "Removed {removed} file{}, freed {}.",
                    if removed == 1 { "" } else { "s" },
                    blindrss_core::play_cache::human_size(freed)
                ),
                "Playback cache cleared",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
            .build();
            md.show_modal();
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
            let redirect_uri = trimmed_redirect(&ino_redirect);
            inoreader_listener_connect(&frame_ls, &path_ls, app_id, app_key, redirect_uri);
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
        let redirect_uri = trimmed_redirect(&ino_redirect);
        let url = blindrss_core::oauth::authorization_url(&app_id, &redirect_uri, &state);
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
        match blindrss_core::oauth::exchange_code(&app_id, &app_key, &code, &redirect_uri) {
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
                        serde_json::json!(blindrss_core::oauth::token_expires_at_from(exp, now)),
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
