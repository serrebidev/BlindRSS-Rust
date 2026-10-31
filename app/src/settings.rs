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

/// Shows the settings dialog. Returns the name of a maintenance
/// action the user chose inside the dialog ("backup", "restore",
/// "takeout", "archive", "sync", "prune", "update"), or None. The
/// caller runs the action after the dialog has closed, so the
/// action's own dialogs parent cleanly on the main frame.
pub fn show_settings(frame: &Frame, config_path: &Path) -> Option<&'static str> {
    let cfg = Config::load(config_path).unwrap_or_default();
    let dialog = Dialog::builder(frame, "BlindRSS Settings").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

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

    // Maintenance actions (moved off the main window, Brandon
    // 2026-10-30): each saves the fields, closes the dialog, and
    // reports its name back so the caller runs it on the main frame.
    let intent: std::rc::Rc<std::cell::RefCell<Option<&'static str>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let maint_label = StaticText::builder(&dialog)
        .with_label("Maintenance")
        .build();
    sizer.add(&maint_label, 0, SizerFlag::All, 4);
    let maint_row1 = BoxSizer::builder(Orientation::Horizontal).build();
    let maint_row2 = BoxSizer::builder(Orientation::Horizontal).build();
    let backup_btn = Button::builder(&dialog).with_label("Backup Now").build();
    let restore_btn = Button::builder(&dialog)
        .with_label("Restore Backup...")
        .build();
    let takeout_btn = Button::builder(&dialog)
        .with_label("Import YouTube Takeout...")
        .build();
    let archive_btn = Button::builder(&dialog)
        .with_label("Rescan Podcast Archive")
        .build();
    let sync_btn = Button::builder(&dialog)
        .with_label("Sync Providers Now")
        .build();
    let prune_btn = Button::builder(&dialog)
        .with_label("Prune Old Articles")
        .build();
    let update_btn = Button::builder(&dialog)
        .with_label("Check for Updates")
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
    let connect_btn = Button::builder(&dialog)
        .with_label("Connect Inoreader...")
        .build();
    let save_btn = Button::builder(&dialog).with_label("Save").build();
    let cancel_btn = Button::builder(&dialog).with_label("Cancel").build();
    btn_row.add(&connect_btn, 0, SizerFlag::All, 4);
    btn_row.add(&save_btn, 0, SizerFlag::All, 4);
    btn_row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 4);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 620));

    let cfg_path_buf = config_path.to_path_buf();
    let cfg_path_connect = config_path.to_path_buf();
    // One save routine shared by Save and the maintenance buttons.
    let cfg_rc = std::rc::Rc::new(std::cell::RefCell::new(cfg));
    let save_all: std::rc::Rc<dyn Fn()> = {
        let cfg_s = cfg_rc.clone();
        let path_s = cfg_path_buf.clone();
        std::rc::Rc::new(move || {
            let mut cfg = cfg_s.borrow_mut();
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
            let _ = cfg.save(&path_s);
        })
    };
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
