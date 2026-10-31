//! YouTube account sign-in + subscription sync (Python
//! `core/youtube_account.py` + the factory's
//! YouTubeAccountProvider wrapper).
//!
//! Sign-in is the device-code flow the YouTube TV app uses:
//! the user approves at google.com/device with a short code;
//! no password passes through the app. Python ships the TV
//! client's public client id; the client secret for that
//! public installed-app client is read from config
//! (`youtube_account_client_secret`) rather than embedded.
//!
//! When signed in, the account's subscriptions auto-add as
//! ordinary YouTube feeds under the configured category
//! (youtube_account_auto_add / youtube_account_category),
//! tracked in youtube_account_known_channels so each sync
//! only adds what is new.

/// The YouTube TV client's public client id (shipped by
/// Python; also used by yt-dlp's TV client).
pub const CLIENT_ID: &str =
    "861556708454-d6dlm3lh05idd8npek18k6be8ba3oc68.apps.googleusercontent.com";

const DEVICE_CODE_URL: &str = "https://www.youtube.com/o/oauth2/device/code";
const TOKEN_URL: &str = "https://www.youtube.com/o/oauth2/token";
const SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";
const GRANT_TYPE: &str = "http://oauth.net/grant_type/device/1.0";

/// What starting a sign-in returns for the user to act on.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSignIn {
    pub user_code: String,
    pub verification_url: String,
    pub device_code: String,
    pub interval: u64,
    pub expires_in: u64,
}

fn form_post(url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let mut resp = crate::http::agent()
        .post(url)
        .send_form(form.iter().copied())
        .map_err(|e| format!("request failed: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read failed: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("bad response: {e}"))
}

/// Begin a device sign-in (POST device/code).
pub fn start_sign_in(client_id: &str) -> Result<DeviceSignIn, String> {
    let device_id = uuid::Uuid::new_v4().to_string();
    let value = form_post(
        DEVICE_CODE_URL,
        &[
            ("client_id", client_id),
            ("scope", SCOPE),
            ("device_id", &device_id),
            ("device_model", "ytlr::"),
        ],
    )?;
    Ok(DeviceSignIn {
        user_code: value
            .get("user_code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        verification_url: value
            .get("verification_url")
            .and_then(|v| v.as_str())
            .unwrap_or("https://www.google.com/device")
            .to_string(),
        device_code: value
            .get("device_code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        interval: value.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
        expires_in: value
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(1800),
    })
}

/// One poll of the token endpoint: Ok(Some(refresh_token))
/// when approved, Ok(None) while still pending, Err on a
/// terminal failure (expired/denied).
pub fn poll_sign_in(
    client_id: &str,
    client_secret: &str,
    device_code: &str,
) -> Result<Option<String>, String> {
    let value = form_post(
        TOKEN_URL,
        &[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", device_code),
            ("grant_type", GRANT_TYPE),
        ],
    )?;
    if let Some(rt) = value.get("refresh_token").and_then(|v| v.as_str()) {
        return Ok(Some(rt.to_string()));
    }
    let error = value
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match error.as_str() {
        "authorization_pending" | "slow_down" => Ok(None),
        "" => Ok(None),
        other => Err(format!("sign-in failed: {other}")),
    }
}

/// Exchange a refresh token for an access token.
pub fn access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String, String> {
    let value = form_post(
        TOKEN_URL,
        &[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    value
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no access_token in the token response".to_string())
}

/// The account's subscriptions: (channel id, channel title)
/// via the YouTube Data API (paged).
pub fn fetch_subscriptions(access: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut page_token = String::new();
    loop {
        let mut url = "https://www.googleapis.com/youtube/v3/subscriptions?part=snippet&mine=true&maxResults=50".to_string();
        if !page_token.is_empty() {
            url.push_str(&format!("&pageToken={page_token}"));
        }
        let mut resp = crate::http::agent()
            .get(&url)
            .header("Authorization", &format!("Bearer {access}"))
            .call()
            .map_err(|e| format!("subscriptions failed: {e}"))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("bad response: {e}"))?;
        if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let snippet = item.get("snippet").cloned().unwrap_or_default();
                let channel_id = snippet
                    .get("resourceId")
                    .and_then(|r| r.get("channelId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = snippet
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !channel_id.is_empty() {
                    out.push((channel_id, title));
                }
            }
        }
        match value.get("nextPageToken").and_then(|v| v.as_str()) {
            Some(next) if !next.is_empty() => page_token = next.to_string(),
            _ => break,
        }
    }
    Ok(out)
}

/// Sync the account's subscriptions into the database as
/// feeds (the provider wrapper's auto-add). Returns how many
/// feeds were newly added. Updates the known-channels list
/// and last-sync stamp in config.
pub fn sync_account(
    cfg: &mut crate::config::Config,
    config_path: &std::path::Path,
    db: &crate::db::Db,
) -> Result<usize, String> {
    let refresh = cfg.get_str("youtube_account_refresh_token", "").to_string();
    if refresh.is_empty() {
        return Err("YouTube account is not signed in".to_string());
    }
    let client_id = {
        let v = cfg.get_str("youtube_account_client_id", "");
        if v.is_empty() { CLIENT_ID } else { v }.to_string()
    };
    let secret = cfg.get_str("youtube_account_client_secret", "").to_string();
    let access = access_token(&client_id, &secret, &refresh)?;
    let subs = fetch_subscriptions(&access)?;
    let known: Vec<String> = cfg
        .raw()
        .get("youtube_account_known_channels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let category = cfg
        .get_str("youtube_account_category", "YouTube")
        .to_string();
    let auto_add = cfg.get_bool("youtube_account_auto_add", true);
    let mut added = 0;
    let mut now_known: Vec<String> = known.clone();
    for (channel_id, title) in &subs {
        if !now_known.contains(channel_id) {
            now_known.push(channel_id.clone());
        }
        if auto_add && !known.contains(channel_id) {
            let url = format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}");
            if !db.feed_url_exists(&url).unwrap_or(false) {
                db.add_feed(&url, title, Some(&category))
                    .map_err(|e| format!("{e}"))?;
                added += 1;
            }
        }
    }
    cfg.set_path(
        "youtube_account_known_channels",
        serde_json::json!(now_known),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    cfg.set_path("youtube_account_last_sync", serde_json::json!(now));
    let _ = cfg.save(config_path);
    Ok(added)
}
