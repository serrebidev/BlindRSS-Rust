//! Synthetic media subscriptions, following the Python source adapters.

use crate::extract::{attr_value, find_elements, has_class, leaf_text};
use crate::feeds::{ParsedEntry, ParsedFeed};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CONFIG: Mutex<Option<PathBuf>> = Mutex::new(None);
static SOUNDCLOUD_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn configure(path: &Path) {
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_owned());
}

pub(crate) fn config() -> crate::config::Config {
    CONFIG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|path| crate::config::Config::load(path).ok())
        .unwrap_or_default()
}

fn host_is(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Normalize only listing pages; single tracks/videos remain articles.
pub fn subscription_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let parts: Vec<_> = url
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let listing = if host_is(&host, "soundcloud.com") && host != "feeds.soundcloud.com" {
        !parts.is_empty()
            && ![
                "you", "search", "discover", "stream", "upload", "settings", "pages", "tags",
            ]
            .contains(&parts[0])
            && (parts.len() == 1 || parts.contains(&"sets"))
    } else if host_is(&host, "mixcloud.com") {
        parts.len() == 1 || parts.len() >= 3 && parts[1] == "playlists"
    } else if host_is(&host, "rumble.com") {
        if parts.len() == 2 && matches!(parts[0], "c" | "user") {
            url.set_path(&format!("{}/videos", url.path().trim_end_matches('/')));
        }
        {
            let p = url.path().to_ascii_lowercase();
            !(p.ends_with(".html")
                || p.ends_with(".xml")
                || p.ends_with(".rss")
                || p.ends_with(".atom"))
        }
    } else if host_is(&host, "odysee.com") || host_is(&host, "lbry.tv") {
        parts.len() == 1 && parts[0].starts_with('@')
    } else {
        false
    };
    if !listing {
        return None;
    }
    url.set_fragment(None);
    if host_is(&host, "rumble.com") && url.path().starts_with("/search") {
        if !url.query_pairs().any(|(key, _)| key == "sort") {
            url.query_pairs_mut().append_pair("sort", "date");
        }
    } else {
        url.set_query(None);
    }
    Some(url.to_string().trim_end_matches('/').to_string())
}

pub fn youtube_uploads_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !host_is(url.host_str()?, "youtube.com") || url.path() != "/feeds/videos.xml" {
        return None;
    }
    for (key, value) in url.query_pairs() {
        if key == "channel_id"
            && value.starts_with("UC")
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Some(format!(
                "https://www.youtube.com/playlist?list=UU{}",
                &value[2..]
            ));
        }
        if key == "playlist_id" {
            return Some(format!("https://www.youtube.com/playlist?list={value}"));
        }
        if key == "user" {
            return Some(format!("https://www.youtube.com/user/{value}/videos"));
        }
    }
    None
}

fn field<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| {
            v.get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
}

fn timestamp(seconds: i64) -> String {
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    let time = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

fn entry(v: &Value, fallback_author: &str) -> Option<ParsedEntry> {
    let mut address =
        field(v, &["permalink_url", "webpage_url", "url", "original_url"]).to_string();
    if !address.starts_with("http://") && !address.starts_with("https://") {
        let ie = field(v, &["ie_key", "extractor_key"]);
        if ie.to_ascii_lowercase().contains("youtube") && !address.is_empty() {
            address = format!("https://www.youtube.com/watch?v={address}");
        } else {
            return None;
        }
    }
    let mut url = url::Url::parse(&address).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if url.host_str().is_some_and(|host| {
        host_is(host, "rumble.com") || host_is(host, "odysee.com") || host_is(host, "lbry.tv")
    }) {
        url.set_query(None);
        url.set_fragment(None);
    }
    let address = url.to_string();
    let title = field(v, &["title", "name"]);
    let author = field(v, &["channel", "uploader", "author"]);
    let author = if author.is_empty() {
        v.get("user")
            .map(|u| field(u, &["username", "name"]))
            .unwrap_or(fallback_author)
    } else {
        author
    };
    let date = ["timestamp", "release_timestamp"]
        .iter()
        .find_map(|key| v.get(*key).and_then(Value::as_i64))
        .map(timestamp)
        .or_else(|| {
            let value = field(
                v,
                &[
                    "created_at",
                    "display_date",
                    "created_time",
                    "published",
                    "upload_date",
                    "release_date",
                ],
            );
            if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
                Some(format!(
                    "{}-{}-{}T00:00:00Z",
                    &value[..4],
                    &value[4..6],
                    &value[6..]
                ))
            } else {
                (!value.is_empty()).then(|| value.to_string())
            }
        });
    let description = field(v, &["description", "summary"]).to_string();
    Some(ParsedEntry {
        id: address.clone(),
        title: if title.is_empty() {
            address.clone()
        } else {
            title.into()
        },
        url: address.clone(),
        content_text: description.clone(),
        summary: description,
        date,
        author: (!author.is_empty()).then(|| author.to_string()),
        media_url: Some(address),
        media_type: None,
        tags: Vec::new(),
    })
}

pub fn parse_listing(value: &Value, source: &str, limit: usize) -> ParsedFeed {
    let title = field(value, &["title", "username", "name", "channel", "uploader"]);
    let mut feed = ParsedFeed {
        title: if title.is_empty() {
            source.into()
        } else {
            title.into()
        },
        entries: Vec::new(),
    };
    let rows = value
        .get("entries")
        .or_else(|| value.get("collection"))
        .or_else(|| value.get("data"))
        .or_else(|| value.get("tracks"))
        .and_then(Value::as_array);
    let mut seen = HashSet::new();
    if let Some(rows) = rows {
        for row in rows {
            if let Some(item) = entry(row, &feed.title)
                && seen.insert(item.url.clone())
            {
                feed.entries.push(item);
                if feed.entries.len() >= limit {
                    break;
                }
            }
        }
    } else if let Some(item) = entry(value, &feed.title) {
        feed.entries.push(item);
    }
    feed
}

pub fn parse_rumble_listing(html: &str, source: &str, limit: usize) -> ParsedFeed {
    let title = find_elements(html, Some("title"), &|_| true)
        .first()
        .map(|s| leaf_text(&html[s.inner_start..s.inner_end]))
        .unwrap_or_else(|| "Rumble".into());
    let mut feed = ParsedFeed {
        title,
        entries: Vec::new(),
    };
    let Ok(base) = url::Url::parse(source) else {
        return feed;
    };
    let mut seen = HashSet::new();
    let cards = find_elements(html, None, &|tag| {
        attr_value(tag, "class").is_some_and(|c| c.contains("videostream"))
    });
    let sections: Vec<_> = cards
        .iter()
        .map(|s| &html[s.start..s.end])
        .chain(std::iter::once(html))
        .collect();
    for section in sections {
        for span in find_elements(section, Some("a"), &|tag| attr_value(tag, "href").is_some()) {
            let tag = &section[span.start..span.inner_start];
            let Some(href) = attr_value(tag, "href") else {
                continue;
            };
            let Ok(mut url) = base.join(&href) else {
                continue;
            };
            if url.host_str() != base.host_str()
                || !url.path().starts_with("/v")
                || url.path().starts_with("/videos")
                || !url.path().ends_with(".html")
            {
                continue;
            }
            url.set_query(None);
            url.set_fragment(None);
            if !seen.insert(url.to_string()) {
                continue;
            }
            let title = find_elements(section, None, &|t| has_class(t, "thumbnail__title"))
                .first()
                .map(|s| leaf_text(&section[s.inner_start..s.inner_end]))
                .filter(|s| !s.is_empty())
                .or_else(|| attr_value(tag, "title"))
                .unwrap_or_else(|| leaf_text(&section[span.inner_start..span.inner_end]));
            let date = find_elements(section, Some("time"), &|_| true)
                .first()
                .and_then(|s| attr_value(&section[s.start..s.inner_start], "datetime"));
            if let Some(item) = entry(
                &serde_json::json!({"url":url.to_string(), "title":title, "published":date}),
                "Rumble",
            ) {
                feed.entries.push(item);
                if feed.entries.len() >= limit {
                    return feed;
                }
            }
        }
    }
    feed
}

fn text_get(url: &str) -> Result<String, String> {
    let path = CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let jar = path
        .as_ref()
        .map(|p| crate::cookies::load_jar_beside(p))
        .unwrap_or_default();
    let cookie = crate::cookies::cookie_header_for(&jar, url);
    crate::extract::fetch_html_with_cookies(url, &cookie)
}

fn json_get(url: &str) -> Result<Value, String> {
    serde_json::from_str(&text_get(url)?).map_err(|e| format!("Source returned invalid JSON: {e}"))
}

/// Bounded external yt-dlp execution; never holds Python's GIL or runs on the UI thread.
pub(crate) fn ytdlp_json(target: &str, limit: usize) -> Result<Value, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp was not found")?;
    let mut command = std::process::Command::new(exe);
    command.args([
        "--no-warnings",
        "--skip-download",
        "--flat-playlist",
        "--dump-single-json",
        "--playlist-end",
        &limit.clamp(1, 1000).to_string(),
        "--socket-timeout",
        "20",
        "--user-agent",
        &crate::http::user_agent(),
    ]);
    if let Some(cookies) =
        std::env::var_os("BLINDRSS_YTDLP_COOKIES").filter(|p| Path::new(p).is_file())
    {
        command.arg("--cookies").arg(cookies);
    }
    command
        .arg("--")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|e| format!("yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or("yt-dlp output was unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("yt-dlp errors were unavailable")?;
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => {
                #[cfg(windows)]
                {
                    let mut kill = std::process::Command::new("taskkill");
                    use std::os::windows::process::CommandExt;
                    let _ = kill
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .creation_flags(0x0800_0000)
                        .output();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err("Source listing timed out".into());
            }
        }
    };
    let bytes = output
        .join()
        .map_err(|_| "Source output worker stopped")?
        .map_err(|e| e.to_string())?;
    let errors = errors.join().ok().and_then(Result::ok).unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "Source listing failed: {}",
            String::from_utf8_lossy(&errors)
                .lines()
                .last()
                .unwrap_or("yt-dlp failed")
        ));
    }
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Source listing was too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid source listing: {e}"))
}

pub(crate) fn soundcloud_get(path: &str, query: &[(&str, String)]) -> Result<Value, String> {
    let assets =
        regex::Regex::new(r#"https://[a-z0-9-]+\.sndcdn\.com/assets/[^\"']+\.js"#).unwrap();
    let token = regex::Regex::new(r#"client_id[:=]\s*[\"']([0-9A-Za-z]{20,})[\"']"#).unwrap();
    for attempt in 0..2 {
        let mut id = SOUNDCLOUD_ID
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if id.is_none() || attempt == 1 {
            let html = text_get("https://soundcloud.com/")?;
            for asset in assets
                .find_iter(&html)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .take(30)
            {
                if let Ok(js) = text_get(asset.as_str())
                    && let Some(found) = token.captures(&js)
                {
                    id = Some(found[1].to_string());
                    break;
                }
            }
            *SOUNDCLOUD_ID.lock().unwrap_or_else(|e| e.into_inner()) = id.clone();
        }
        let id = id.ok_or("SoundCloud did not publish a usable client ID")?;
        let mut url = url::Url::parse(&format!("https://api-v2.soundcloud.com{path}"))
            .map_err(|e| e.to_string())?;
        for (key, value) in query {
            if *key != "client_id" {
                url.query_pairs_mut().append_pair(key, value);
            }
        }
        url.query_pairs_mut().append_pair("client_id", &id);
        match json_get(url.as_str()) {
            Ok(value) => return Ok(value),
            Err(error) if attempt == 1 => return Err(error),
            _ => {}
        }
    }
    Err("SoundCloud listing failed".into())
}

fn soundcloud_listing(source: &str, limit: usize) -> Result<ParsedFeed, String> {
    let resolved = soundcloud_get("/resolve", &[("url", source.into())])?;
    if resolved.get("kind").and_then(Value::as_str) == Some("user") {
        let id = resolved
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("SoundCloud user has no identifier")?;
        let mut page = soundcloud_get(
            &format!("/users/{id}/tracks"),
            &[
                ("limit", limit.min(200).to_string()),
                ("linked_partitioning", "1".into()),
            ],
        )?;
        let mut feed = ParsedFeed {
            title: field(&resolved, &["username"]).into(),
            entries: Vec::new(),
        };
        let mut seen = HashSet::new();
        for _ in 0..8 {
            feed.entries.extend(
                parse_listing(&page, source, limit)
                    .entries
                    .into_iter()
                    .filter(|e| seen.insert(e.url.clone()))
                    .take(limit - feed.entries.len()),
            );
            if feed.entries.len() >= limit {
                break;
            }
            let Some(next) = page
                .get("next_href")
                .and_then(Value::as_str)
                .and_then(|u| url::Url::parse(u).ok())
            else {
                break;
            };
            if next.host_str() != Some("api-v2.soundcloud.com") {
                break;
            }
            let query: Vec<_> = next
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let borrowed: Vec<_> = query.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            page = soundcloud_get(next.path(), &borrowed)?;
        }
        return Ok(feed);
    }
    let tracks = resolved
        .get("tracks")
        .and_then(Value::as_array)
        .ok_or("SoundCloud playlist has no tracks")?;
    let mut hydrated: std::collections::HashMap<u64, Value> = tracks
        .iter()
        .filter_map(|v| {
            v.get("id")
                .and_then(Value::as_u64)
                .map(|id| (id, v.clone()))
        })
        .collect();
    let missing: Vec<_> = tracks
        .iter()
        .take(limit)
        .filter(|v| field(v, &["title", "permalink_url"]).is_empty())
        .filter_map(|v| v.get("id").and_then(Value::as_u64))
        .collect();
    for batch in missing.chunks(50) {
        let values = soundcloud_get(
            "/tracks",
            &[(
                "ids",
                batch
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )],
        )?;
        for value in values.as_array().into_iter().flatten() {
            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                hydrated.insert(id, value.clone());
            }
        }
    }
    let rows: Vec<_> = tracks
        .iter()
        .take(limit)
        .filter_map(|v| {
            v.get("id")
                .and_then(Value::as_u64)
                .and_then(|id| hydrated.get(&id))
                .cloned()
        })
        .collect();
    Ok(parse_listing(
        &serde_json::json!({"title":field(&resolved,&["title"]), "entries":rows}),
        source,
        limit,
    ))
}

fn mixcloud_listing(source: &str, limit: usize) -> Result<ParsedFeed, String> {
    let source = url::Url::parse(source).map_err(|e| e.to_string())?;
    let key = format!("{}/", source.path().trim_end_matches('/'));
    let meta = json_get(&format!("https://api.mixcloud.com{key}"))?;
    let mut feed = ParsedFeed {
        title: field(&meta, &["name"]).into(),
        entries: Vec::new(),
    };
    let endpoint = format!("https://api.mixcloud.com{key}cloudcasts/");
    let mut url = format!("{endpoint}?limit={}", limit.min(100));
    let mut seen = HashSet::new();
    for _ in 0..6 {
        let page = json_get(&url)?;
        feed.entries.extend(
            parse_listing(&page, source.as_str(), limit)
                .entries
                .into_iter()
                .filter(|e| seen.insert(e.url.clone()))
                .take(limit - feed.entries.len()),
        );
        if feed.entries.len() >= limit {
            break;
        }
        let Some(next) = page
            .pointer("/paging/next")
            .and_then(Value::as_str)
            .and_then(|u| url::Url::parse(u).ok())
        else {
            break;
        };
        let Some(offset) = next
            .query_pairs()
            .find(|(key, _)| key == "offset")
            .map(|(_, v)| v.into_owned())
        else {
            break;
        };
        let mut next = url::Url::parse(&endpoint).unwrap();
        next.query_pairs_mut()
            .append_pair("limit", &(limit - feed.entries.len()).min(100).to_string())
            .append_pair("offset", &offset);
        url = next.into();
    }
    Ok(feed)
}

pub fn fetch_feed(value: &str, initial: bool) -> Option<Result<ParsedFeed, String>> {
    let source = subscription_url(value)?;
    Some((|| {
        let cfg = config();
        let host = url::Url::parse(&source)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        let platform = if host_is(&host, "soundcloud.com") {
            "soundcloud"
        } else if host_is(&host, "mixcloud.com") {
            "mixcloud"
        } else if host_is(&host, "rumble.com") {
            "rumble"
        } else {
            "odysee"
        };
        let limit = cfg
            .get_i64(
                &format!(
                    "{platform}_max_items_{}",
                    if initial { "initial" } else { "refresh" }
                ),
                if initial { 150 } else { 60 },
            )
            .clamp(1, 1000) as usize;
        if platform == "mixcloud" {
            return mixcloud_listing(&source, limit.min(200));
        }
        if platform == "soundcloud"
            && let Ok(feed) = soundcloud_listing(&source, limit.min(300))
            && !feed.entries.is_empty()
        {
            return Ok(feed);
        }
        if platform == "rumble" {
            let pages = cfg
                .get_i64(
                    if initial {
                        "rumble_max_pages_initial"
                    } else {
                        "rumble_max_pages_refresh"
                    },
                    if initial { 3 } else { 1 },
                )
                .clamp(1, 20);
            let mut feed = ParsedFeed {
                title: String::new(),
                entries: Vec::new(),
            };
            let mut seen = HashSet::new();
            for page in 1..=pages {
                let mut url = url::Url::parse(&source).unwrap();
                if page > 1 {
                    url.query_pairs_mut().append_pair("page", &page.to_string());
                }
                let parsed = parse_rumble_listing(&text_get(url.as_str())?, &source, limit);
                if feed.title.is_empty() {
                    feed.title = parsed.title;
                }
                let before = feed.entries.len();
                feed.entries.extend(
                    parsed
                        .entries
                        .into_iter()
                        .filter(|e| seen.insert(e.url.clone()))
                        .take(limit - before),
                );
                if feed.entries.len() == before || feed.entries.len() >= limit {
                    break;
                }
            }
            return Ok(feed);
        }
        Ok(parse_listing(&ytdlp_json(&source, limit)?, &source, limit))
    })())
}

pub fn youtube_fallback(url: &str, initial: bool) -> Option<Result<ParsedFeed, String>> {
    let listing = youtube_uploads_url(url)?;
    let cfg = config();
    let limit = cfg
        .get_i64(
            if initial {
                "youtube_max_items_initial"
            } else {
                "youtube_max_items_refresh"
            },
            if initial { 300 } else { 30 },
        )
        .clamp(1, 1000) as usize;
    Some(ytdlp_json(&listing, limit).map(|value| parse_listing(&value, &listing, limit)))
}
