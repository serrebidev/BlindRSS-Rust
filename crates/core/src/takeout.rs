//! YouTube Takeout import (port of `core/youtube_takeout.py`).
//!
//! Reads a Google Takeout ZIP and returns the channel and
//! playlist feeds from subscriptions.csv, channel.csv,
//! playlists.csv and watch-history.html under
//! "YouTube and YouTube Music". Deduplication is per
//! (source, URL) — the same channel legitimately appears once
//! per source it was found in, and the per-source counts are
//! part of the result (Audit 8 Task 27, F11). The old global
//! URL dedupe silently dropped those later sightings and with
//! them the source provenance the import dialog reports.

use std::collections::HashSet;
use std::io::Read;

#[derive(Debug, Clone, PartialEq)]
pub struct TakeoutFeed {
    pub title: String,
    pub url: String,
    /// One of "subscriptions", "history", "owner", "playlists".
    pub source: String,
}

/// The parse result: every (source, URL)-unique feed plus the
/// per-source counts (Python's `TakeoutImport`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TakeoutImport {
    pub feeds: Vec<TakeoutFeed>,
    pub subscriptions: usize,
    pub history_channels: usize,
    pub owner_channels: usize,
    pub playlists: usize,
}

impl TakeoutImport {
    pub fn count(&self, source: &str) -> usize {
        match source {
            "subscriptions" => self.subscriptions,
            "history" => self.history_channels,
            "owner" => self.owner_channels,
            "playlists" => self.playlists,
            _ => 0,
        }
    }

    /// Source groups with content, in the import dialog's
    /// order (Python `mainframe._show_youtube_takeout_options`).
    pub fn groups(&self) -> Vec<&'static str> {
        ["subscriptions", "history", "owner", "playlists"]
            .into_iter()
            .filter(|source| self.count(source) > 0)
            .collect()
    }

    /// Python `TakeoutImport.selected`: keep only the chosen
    /// sources; the returned counts are tallied over the kept
    /// feeds BEFORE URL deduplication, while the feeds
    /// themselves are URL-unique, first occurrence winning.
    pub fn selected(&self, sources: &[&str]) -> TakeoutImport {
        let mut out = TakeoutImport::default();
        let mut seen_urls: HashSet<&str> = HashSet::new();
        for feed in &self.feeds {
            if !sources.contains(&feed.source.as_str()) {
                continue;
            }
            match feed.source.as_str() {
                "subscriptions" => out.subscriptions += 1,
                "history" => out.history_channels += 1,
                "owner" => out.owner_channels += 1,
                "playlists" => out.playlists += 1,
                _ => {}
            }
            if seen_urls.insert(feed.url.as_str()) {
                out.feeds.push(feed.clone());
            }
        }
        out
    }

    /// Python `write_takeout_opml` as a string: a flat OPML
    /// document (head title "YouTube Takeout"), one outline per
    /// URL-unique feed carrying text/title/type/xmlUrl.
    pub fn to_opml(&self) -> String {
        let mut out = String::from(
            "<?xml version='1.0' encoding='utf-8'?>\n<opml version=\"2.0\"><head><title>YouTube Takeout</title></head><body>",
        );
        let mut seen: HashSet<&str> = HashSet::new();
        for feed in &self.feeds {
            if !seen.insert(feed.url.as_str()) {
                continue;
            }
            out.push_str(&format!(
                "<outline text=\"{}\" title=\"{}\" type=\"rss\" xmlUrl=\"{}\" />",
                escape_attr(&feed.title),
                escape_attr(&feed.title),
                escape_attr(&feed.url),
            ));
        }
        out.push_str("</body></opml>");
        out
    }
}

/// Attribute escaping in ElementTree's shape (what Python's
/// `write_takeout_opml` emits): the markup characters plus
/// tab/newline/return as character references.
fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#09;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
    out
}

fn channel_feed(channel_id: &str) -> String {
    let value = channel_id.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!("https://www.youtube.com/feeds/videos.xml?channel_id={value}")
    }
}

fn playlist_feed(playlist_id: &str) -> String {
    let value = playlist_id.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!("https://www.youtube.com/feeds/videos.xml?playlist_id={value}")
    }
}

/// Python `_channel_id_from_url`: the segment after /channel/
/// on a YouTube host, taken as-is (no length or charset gate).
fn channel_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(
        parsed.host_str()?.to_lowercase().as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com"
    ) {
        return None;
    }
    let parts: Vec<&str> = parsed
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("channel") {
        Some(parts[1].to_string())
    } else {
        None
    }
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

/// One CSV member through Python's DictReader shape: the first
/// row names the columns and lookups use the exact header
/// names Python reads. Returns (id, title) pairs.
fn csv_id_title(text: &str, id_header: &str, title_header: &str) -> Vec<(String, String)> {
    let rows = csv_rows(text);
    let Some(header) = rows.first() else {
        return Vec::new();
    };
    let id_col = header.iter().position(|h| h == id_header);
    let title_col = header.iter().position(|h| h == title_header);
    let get = |row: &Vec<String>, col: Option<usize>| {
        col.and_then(|i| row.get(i)).cloned().unwrap_or_default()
    };
    rows[1..]
        .iter()
        .map(|row| (get(row, id_col), get(row, title_col)))
        .collect()
}

/// Python `_read_member`: the size cap applies, decoding is
/// utf-8-sig with replacement.
fn read_member(reader: &mut impl Read, name: &str, size: u64) -> Result<String, String> {
    if size > 64 * 1024 * 1024 {
        return Err(format!("Takeout member is too large: {name}"));
    }
    let mut bytes = Vec::new();
    reader
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read the Takeout archive: {e}"))?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(format!("Takeout member is too large: {name}"));
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Parse a Takeout ZIP file's bytes (Python
/// `parse_youtube_takeout`, which takes a path; the app reads
/// the file on a worker and hands over the bytes).
pub fn parse_takeout_zip(bytes: &[u8]) -> Result<TakeoutImport, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|_| "The selected file is not a valid ZIP archive.".to_string())?;
    if archive.len() > 20_000 {
        return Err("The selected archive contains too many files.".to_string());
    }
    // Python considers only members under a
    // "YouTube and YouTube Music" section anywhere in the path.
    let mut youtube_indexes = Vec::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("Could not read the Takeout archive: {e}"))?;
        let probe = format!("/{}", file.name().replace('\\', "/").to_lowercase());
        if probe.contains("/youtube and youtube music/") {
            youtube_indexes.push(i);
        }
    }
    if youtube_indexes.is_empty() {
        return Err("No YouTube and YouTube Music export was found in this archive.".to_string());
    }
    let mut out = TakeoutImport::default();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    // Python's add(): (source, URL) dedupe, the title falls
    // back to the feed URL itself, and counts increment per
    // accepted add.
    let add = |out: &mut TakeoutImport,
               seen: &mut HashSet<(String, String)>,
               title: &str,
               url: String,
               source: &str| {
        let url = url.trim().to_string();
        if url.is_empty() || !seen.insert((source.to_string(), url.clone())) {
            return;
        }
        let title = title.trim();
        out.feeds.push(TakeoutFeed {
            title: if title.is_empty() {
                url.clone()
            } else {
                title.to_string()
            },
            url,
            source: source.to_string(),
        });
        match source {
            "subscriptions" => out.subscriptions += 1,
            "history" => out.history_channels += 1,
            "owner" => out.owner_channels += 1,
            "playlists" => out.playlists += 1,
            _ => {}
        }
    };
    for i in youtube_indexes {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Could not read the Takeout archive: {e}"))?;
        let display_name = file.name().to_string();
        let name = display_name.replace('\\', "/").to_lowercase();
        let size = file.size();
        if name.ends_with("/subscriptions/subscriptions.csv") {
            let text = read_member(&mut file, &display_name, size)?;
            for (id, title) in csv_id_title(&text, "Channel ID", "Channel title") {
                add(
                    &mut out,
                    &mut seen,
                    &title,
                    channel_feed(&id),
                    "subscriptions",
                );
            }
        } else if name.ends_with("/channels/channel.csv") {
            let text = read_member(&mut file, &display_name, size)?;
            for (id, title) in csv_id_title(&text, "Channel ID", "Channel title (Original)") {
                add(&mut out, &mut seen, &title, channel_feed(&id), "owner");
            }
        } else if name.ends_with("/playlists/playlists.csv") {
            let text = read_member(&mut file, &display_name, size)?;
            for (id, title) in csv_id_title(&text, "Playlist ID", "Playlist title (original)") {
                add(&mut out, &mut seen, &title, playlist_feed(&id), "playlists");
            }
        } else if name.ends_with("/history/watch-history.html") {
            let text = read_member(&mut file, &display_name, size)?;
            for (channel_id, title) in history_channels(&text) {
                add(
                    &mut out,
                    &mut seen,
                    &title,
                    channel_feed(&channel_id),
                    "history",
                );
            }
        }
    }
    if out.feeds.is_empty() {
        return Err("No subscribable YouTube channels or playlists were found.".to_string());
    }
    Ok(out)
}

/// Channel anchors from watch-history.html (Python's
/// `_HistoryChannelParser`): each anchor whose href resolves
/// to a channel id, titled by its text content (or the id).
fn history_channels(html: &str) -> Vec<(String, String)> {
    static ANCHORS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?is)<a\b[^>]*\bhref\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a\s*>"#)
            .expect("fixed history-link regex")
    });
    let mut channels = Vec::new();
    for link in ANCHORS.captures_iter(html) {
        if let Some(cid) = channel_id_from_url(&link[1]) {
            let title = crate::extract::html_to_text(&link[2]);
            let title = title.trim();
            channels.push((
                cid.clone(),
                if title.is_empty() {
                    cid
                } else {
                    title.to_string()
                },
            ));
        }
    }
    channels
}
