//! YouTube account sign-in + subscription sync (Python
//! `core/youtube_account.py` + the factory's
//! YouTubeAccountProvider wrapper).
//!
//! Sign-in is the device-code flow the YouTube TV app uses:
//! the user approves at google.com/device with a short code;
//! no password passes through the app. Python ships the TV
//! client's public client id; the client secret for that
//! public installed-app client is read from config
//! (`youtube_account_client_secret`) when set; otherwise the
//! YouTube TV app's installed-app client constants are used, as
//! in Python (Google documents that an installed app's client
//! secret is not confidential).
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

/// The YouTube TV installed-app client secret (Python's
/// CLIENT_SECRET; not a confidential credential).
pub const CLIENT_SECRET: &str = "SboVhoG9s0rNafixCSGGKXAT";

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

/// The effective client secret: a config-provided value wins;
/// empty resolves to the built-in TV client secret (Python's
/// flow always uses the constant).
pub fn effective_secret(configured: &str) -> &str {
    if configured.trim().is_empty() {
        CLIENT_SECRET
    } else {
        configured
    }
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
            ("client_secret", effective_secret(client_secret)),
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
            ("client_secret", effective_secret(client_secret)),
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

pub const RECOMMENDATIONS_FEED_ID: &str = "youtube-account:recommendations";
const BROWSE_URL: &str = "https://www.youtube.com/youtubei/v1/browse";

/// Stable per-account key: sha256 of the refresh token (Python's
/// account_key).
pub fn account_key(refresh_token: &str) -> String {
    use sha2::Digest;
    if refresh_token.is_empty() {
        return String::new();
    }
    let digest = sha2::Sha256::digest(refresh_token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn text_runs(text: &serde_json::Value) -> String {
    if let Some(s) = text.get("simpleText").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    text.get("runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|v| v.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

fn valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Current TV home-page videos, preserving YouTube's order —
/// port of Python's parse_recommendations (an iterative walk
/// collecting tileRenderer video tiles).
pub fn parse_recommendations(data: &serde_json::Value) -> Vec<(String, String, String)> {
    let mut found: Vec<(String, String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut stack: Vec<&serde_json::Value> = vec![data];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(tile) = map.get("tileRenderer")
                    && tile.get("contentType").and_then(|v| v.as_str())
                        == Some("TILE_CONTENT_TYPE_VIDEO")
                {
                    let video_id = tile
                        .get("contentId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let metadata = tile
                        .get("metadata")
                        .and_then(|m| m.get("tileMetadataRenderer"));
                    let mut title = metadata
                        .and_then(|m| m.get("title"))
                        .map(text_runs)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let lines: Vec<&serde_json::Value> = metadata
                        .and_then(|m| m.get("lines"))
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().collect())
                        .unwrap_or_default();
                    let line_items = |line: &serde_json::Value| -> Vec<serde_json::Value> {
                        line.get("lineRenderer")
                            .and_then(|l| l.get("items"))
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default()
                    };
                    let mut author = String::new();
                    let overlays = tile
                        .get("header")
                        .and_then(|h| h.get("tileHeaderRenderer"))
                        .and_then(|h| h.get("thumbnailOverlays"))
                        .and_then(|v| v.as_array());
                    let is_podcast = overlays.is_some_and(|list| {
                        list.iter().any(|o| {
                            o.get("thumbnailOverlayTimeStatusRenderer")
                                .and_then(|r| r.get("icon"))
                                .and_then(|i| i.get("iconType"))
                                .and_then(|v| v.as_str())
                                == Some("BROADCAST")
                        })
                    });
                    if let Some(first_line) = lines.first() {
                        let items = line_items(first_line);
                        if let Some(item) = items.first() {
                            let text = item
                                .get("lineItemRenderer")
                                .and_then(|l| l.get("text"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            author = text_runs(&text);
                            // Podcast tiles: series, episode, channel.
                            if is_podcast
                                && lines.len() == 2
                                && items.len() == 1
                                && text.get("simpleText").is_some()
                            {
                                let second = line_items(lines[1]);
                                if second.len() == 1
                                    && let Some(channel) = second[0]
                                        .get("lineItemRenderer")
                                        .and_then(|l| l.get("text"))
                                        .and_then(|t| t.get("simpleText"))
                                        .and_then(|v| v.as_str())
                                    && !channel.is_empty()
                                {
                                    title = author.trim().to_string();
                                    author = channel.trim().to_string();
                                }
                            }
                        }
                    }
                    if valid_video_id(&video_id) && !title.is_empty() && !seen.contains(&video_id) {
                        seen.push(video_id.clone());
                        found.push((video_id, title, author.trim().to_string()));
                    }
                }
                // Push values reversed so popping walks in
                // document order (serde_json preserve_order).
                let values: Vec<&serde_json::Value> = map.values().collect();
                for value in values.into_iter().rev() {
                    stack.push(value);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items.iter().rev() {
                    stack.push(value);
                }
            }
            _ => {}
        }
    }
    found
}

/// Fetch the signed-in account's home recommendations.
pub fn list_recommendations(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<(String, String, String)>, String> {
    let access = access_token(client_id, client_secret, refresh_token)?;
    let body = serde_json::json!({
        "context": {"client": {
            "clientName": "TVHTML5",
            "clientVersion": "7.20250101.00.00",
            "hl": "en",
            "gl": "US",
        }},
        "browseId": "default",
    });
    let agent = crate::http::agent();
    let payload = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let mut resp = agent
        .post(BROWSE_URL)
        .header("Authorization", &format!("Bearer {access}"))
        .header("Content-Type", "application/json")
        .send(payload.as_str())
        .map_err(|e| format!("recommendations request failed: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("recommendations read failed: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("recommendations response was not JSON: {e}"))?;
    let videos = parse_recommendations(&value);
    if videos.is_empty() {
        return Err("YouTube returned no recommendations".to_string());
    }
    Ok(videos)
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
