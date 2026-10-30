//! YouTube Takeout import (port of `core/youtube_takeout.py`).
//!
//! Reads a Google Takeout ZIP and returns the deduplicated channel
//! and playlist feeds from subscriptions.csv, watch-history.html,
//! and playlists/*.csv under "YouTube and YouTube Music".

use std::io::Read;

#[derive(Debug, Clone, PartialEq)]
pub struct TakeoutFeed {
    pub title: String,
    pub url: String,
    pub source: String,
}

fn channel_feed(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

fn playlist_feed(playlist_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?playlist_id={playlist_id}")
}

fn channel_id_from_url(url: &str) -> Option<String> {
    // Channel URLs in Takeout rows look like
    // https://www.youtube.com/channel/UCxxxxxxxx
    let marker = "/channel/";
    let start = url.find(marker)? + marker.len();
    let id: String = url[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.len() >= 10 { Some(id) } else { None }
}

/// Minimal CSV reader: quoted fields, embedded commas/newlines,
/// double-quote escapes. Returns rows of fields.
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut field_started = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' if !field_started => {
                in_quotes = true;
                field_started = true;
            }
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                field_started = false;
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                field_started = false;
                if row.iter().any(|f| !f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            '\r' => {}
            _ => {
                field.push(c);
                field_started = true;
            }
        }
    }
    if field_started || !field.is_empty() || !row.is_empty() {
        row.push(field);
        if row.iter().any(|f| !f.trim().is_empty()) {
            rows.push(row);
        }
    }
    rows
}

fn subscriptions_from_csv(text: &str, out: &mut Vec<TakeoutFeed>, seen: &mut Vec<String>) {
    let rows = csv_rows(text);
    if rows.is_empty() {
        return;
    }
    // Header contains "Channel ID" / "Channel Url" / "Channel Title"
    // in some column order; locate them.
    let header: Vec<String> = rows[0].iter().map(|h| h.trim().to_lowercase()).collect();
    let id_col = header.iter().position(|h| h.contains("channel id"));
    let url_col = header.iter().position(|h| h.contains("channel url"));
    let title_col = header.iter().position(|h| h.contains("channel title"));
    for row in &rows[1..] {
        let get = |idx: Option<usize>| idx.and_then(|i| row.get(i)).map(|s| s.trim().to_string());
        let channel_id = get(id_col)
            .filter(|s| !s.is_empty())
            .or_else(|| get(url_col).and_then(|u| channel_id_from_url(&u)));
        if let Some(cid) = channel_id {
            let feed = channel_feed(&cid);
            if !seen.contains(&feed) {
                seen.push(feed.clone());
                out.push(TakeoutFeed {
                    title: get(title_col).filter(|s| !s.is_empty()).unwrap_or(cid),
                    url: feed,
                    source: "subscriptions".to_string(),
                });
            }
        }
    }
}

fn playlists_from_csv(text: &str, out: &mut Vec<TakeoutFeed>, seen: &mut Vec<String>) {
    // Playlist CSVs list video ids, not the playlist id; the
    // playlist id comes from the file name in parse_takeout.
    let _ = (text, out, seen);
}

/// Parse a Takeout ZIP file's bytes into feed entries.
pub fn parse_takeout_zip(bytes: &[u8]) -> Result<Vec<TakeoutFeed>, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("not a valid ZIP: {e}"))?;
    let mut out: Vec<TakeoutFeed> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut found_youtube = false;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("zip read: {e}"))?;
        let name = file.name().replace('\\', "/").to_lowercase();
        if !name.contains("/youtube and youtube music/")
            && !name.starts_with("youtube and youtube music/")
        {
            continue;
        }
        found_youtube = true;
        if name.ends_with("/subscriptions/subscriptions.csv")
            || name.ends_with("subscriptions.csv") && name.contains("subscriptions")
        {
            let mut text = String::new();
            if file.read_to_string(&mut text).is_ok() {
                subscriptions_from_csv(&text, &mut out, &mut seen);
            }
        } else if name.contains("/playlists/") && name.ends_with(".csv") {
            // Playlist file names look like "Playlist Name.csv" with
            // no id; Takeout also writes "playlists.csv" indexes with
            // a Playlist ID column in newer exports.
            let mut text = String::new();
            if file.read_to_string(&mut text).is_ok() {
                let rows = csv_rows(&text);
                if let Some(header) = rows.first() {
                    let low: Vec<String> = header.iter().map(|h| h.trim().to_lowercase()).collect();
                    if let Some(pid_col) = low.iter().position(|h| h.contains("playlist id")) {
                        let title_col = low
                            .iter()
                            .position(|h| h.contains("playlist title") || h.contains("title"));
                        for row in &rows[1..] {
                            if let Some(pid) =
                                row.get(pid_col).map(|s| s.trim()).filter(|s| !s.is_empty())
                            {
                                let feed = playlist_feed(pid);
                                if !seen.contains(&feed) {
                                    seen.push(feed.clone());
                                    out.push(TakeoutFeed {
                                        title: title_col
                                            .and_then(|c| row.get(c))
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or_else(|| pid.to_string()),
                                        url: feed,
                                        source: "playlists".to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
                playlists_from_csv(&text, &mut out, &mut seen);
            }
        } else if name.ends_with("watch-history.html") {
            let mut text = String::new();
            if file.read_to_string(&mut text).is_ok() {
                // Channels appear as channel links in the history page.
                let mut idx = 0;
                while let Some(pos) = text[idx..].find("/channel/") {
                    let start = idx + pos;
                    if let Some(cid) = channel_id_from_url(&text[start.saturating_sub(40)..]) {
                        let feed = channel_feed(&cid);
                        if !seen.contains(&feed) {
                            seen.push(feed.clone());
                            out.push(TakeoutFeed {
                                title: cid.clone(),
                                url: feed,
                                source: "history".to_string(),
                            });
                        }
                    }
                    idx = start + 9;
                }
            }
        }
    }
    if !found_youtube {
        return Err("No YouTube and YouTube Music export found in this archive".to_string());
    }
    Ok(out)
}
