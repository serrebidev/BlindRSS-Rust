//! Podcast archive rescan (scoped port of `core/podcast_archive.py`).
//!
//! Python's archive follows feed lineage through Wayback snapshots
//! to recover episodes the live feed document no longer carries.
//! Current documents take precedence over older snapshots. Feed migrations
//! are followed with bounded lineage, snapshot count, and document sizes.

use crate::db::{Db, Feed};
use crate::feeds::{ParsedEntry, ParsedFeed};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use ureq::ResponseExt;

#[derive(Debug, Clone)]
pub struct ArchiveEpisode {
    pub entry: ParsedEntry,
    pub source_feed_url: String,
    pub snapshot_timestamp: String,
    pub identity_keys: HashSet<String>,
}

impl ArchiveEpisode {
    pub fn key(&self) -> String {
        use sha2::Digest;
        let seed = if !self.entry.id.is_empty() {
            &self.entry.id
        } else {
            self.entry.media_url.as_ref().unwrap_or(&self.entry.url)
        };
        sha2::Sha256::digest(seed.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveDocument {
    pub title: String,
    pub episodes: Vec<ArchiveEpisode>,
    pub lineage: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub max_snapshots: usize,
    pub include_without_media: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_snapshots: 5000,
            include_without_media: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub feed_title: String,
    pub episodes: Vec<ArchiveEpisode>,
    pub feed_urls: Vec<String>,
    pub snapshots_found: usize,
    pub snapshots_loaded: usize,
    pub failed_snapshots: usize,
    pub canceled: bool,
    pub truncated: bool,
    pub warnings: Vec<String>,
    readable_documents: usize,
    #[cfg(test)]
    documents: Vec<ParsedFeed>,
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub stage: &'static str,
    pub snapshots_found: usize,
    pub snapshots_loaded: usize,
}

fn unwrap_wayback(value: &str) -> &str {
    static WRAPPER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    WRAPPER
        .get_or_init(|| {
            regex::Regex::new(
                r"(?i)^https?://web\.archive\.org/web/\d{1,14}(?:[a-z]{2}_)?/(https?://.+)$",
            )
            .unwrap()
        })
        .captures(value.trim())
        .and_then(|capture| capture.get(1))
        .map_or(value.trim(), |matched| matched.as_str())
}

/// Identity only: never use a canonical URL as a request destination.
pub fn canonical_url(value: &str, media: bool) -> String {
    let value = unwrap_wayback(value);
    let Ok(mut url) = url::Url::parse(value) else {
        return value.into();
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return value.into();
    }
    let host = url
        .host_str()
        .unwrap()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let _ = url.set_host(Some(&host));
    let mut path = String::new();
    for character in url.path().chars() {
        if character != '/' || !path.ends_with('/') {
            path.push(character);
        }
    }
    if !media && path != "/" {
        path.truncate(path.trim_end_matches('/').len());
    }
    url.set_path(&path);
    let mut pairs: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_")
                && !["fbclid", "gclid", "mc_cid", "mc_eid", "ref", "source"].contains(&key.as_str())
                && !(media
                    && [
                        "expires",
                        "key-pair-id",
                        "policy",
                        "signature",
                        "sig",
                        "token",
                        "x-amz-algorithm",
                        "x-amz-credential",
                        "x-amz-date",
                        "x-amz-expires",
                        "x-amz-security-token",
                        "x-amz-signature",
                        "x-amz-signedheaders",
                    ]
                    .contains(&key.as_str()))
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    pairs.sort();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
}

fn identity_keys(
    id: &str,
    page: &str,
    media: Option<&str>,
    title: &str,
    date: Option<&str>,
) -> HashSet<String> {
    let mut keys = HashSet::new();
    if !id.trim().is_empty() {
        keys.insert(format!(
            "guid:{}",
            canonical_url(id.trim(), false).to_lowercase()
        ));
    }
    if !page.trim().is_empty() {
        keys.insert(format!("page:{}", canonical_url(page, false)));
    }
    if let Some(media) = media.filter(|media| !media.trim().is_empty()) {
        keys.insert(format!("media:{}", canonical_url(media, true)));
    }
    let title = title
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(day) = date
        .and_then(|date| date.get(..10))
        .filter(|day| !day.starts_with("0001-"))
        && !title.is_empty()
    {
        keys.insert(format!("title-date:{title}|{day}"));
    }
    keys
}

pub fn article_identity_keys(article: &crate::Article) -> HashSet<String> {
    identity_keys(
        &article.id,
        &article.url,
        article.media_url.as_deref(),
        &article.title,
        article.date.as_deref(),
    )
}

pub fn parse_document(
    document: &[u8],
    source_url: &str,
    timestamp: &str,
) -> Result<ArchiveDocument, String> {
    if document.is_empty() || document.len() > 10 * 1024 * 1024 {
        return Err("Podcast feed document was empty or unexpectedly large".into());
    }
    let feed = crate::feeds::parse_feed(document)?;
    let base = url::Url::parse(unwrap_wayback(source_url)).ok();
    let episodes = feed
        .entries
        .into_iter()
        .map(|mut entry| {
            if let Some(base) = &base {
                if !entry.url.is_empty()
                    && let Ok(url) = base.join(unwrap_wayback(&entry.url))
                {
                    entry.url = url.into();
                }
                if let Some(media) = &entry.media_url
                    && let Ok(url) = base.join(unwrap_wayback(media))
                {
                    entry.media_url = Some(url.into());
                }
            }
            let identity_keys = identity_keys(
                &entry.id,
                &entry.url,
                entry.media_url.as_deref(),
                &entry.title,
                entry.date.as_deref(),
            );
            ArchiveEpisode {
                entry,
                source_feed_url: source_url.into(),
                snapshot_timestamp: timestamp.into(),
                identity_keys,
            }
        })
        .collect();
    Ok(ArchiveDocument {
        title: feed.title,
        episodes,
        lineage: lineage(document, source_url),
    })
}

fn merge_episode(preferred: &mut ArchiveEpisode, other: ArchiveEpisode) {
    fn fill(target: &mut String, other: String) {
        if target.is_empty() {
            *target = other;
        }
    }
    let current = &mut preferred.entry;
    let incoming = other.entry;
    fill(&mut current.id, incoming.id);
    fill(&mut current.title, incoming.title);
    fill(&mut current.url, incoming.url);
    fill(&mut current.content_text, incoming.content_text);
    fill(&mut current.summary, incoming.summary);
    if current
        .date
        .as_deref()
        .is_none_or(|date| date.is_empty() || date.starts_with("0001-"))
    {
        current.date = incoming.date;
    }
    if current.author.as_deref().is_none_or(str::is_empty) {
        current.author = incoming.author;
    }
    if current.media_url.as_deref().is_none_or(str::is_empty) {
        current.media_url = incoming.media_url;
    }
    if current.media_type.as_deref().is_none_or(str::is_empty) {
        current.media_type = incoming.media_type;
    }
    for tag in incoming.tags {
        if !current.tags.contains(&tag) {
            current.tags.push(tag);
        }
    }
    fill(&mut preferred.source_feed_url, other.source_feed_url);
    fill(&mut preferred.snapshot_timestamp, other.snapshot_timestamp);
    preferred.identity_keys.extend(other.identity_keys);
    preferred.identity_keys.extend(identity_keys(
        &current.id,
        &current.url,
        current.media_url.as_deref(),
        &current.title,
        current.date.as_deref(),
    ));
}

pub fn deduplicate_episodes(
    episodes: impl IntoIterator<Item = ArchiveEpisode>,
) -> Vec<ArchiveEpisode> {
    let mut kept: Vec<Option<ArchiveEpisode>> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for mut episode in episodes {
        if episode.identity_keys.is_empty() {
            episode.identity_keys = identity_keys(
                &episode.entry.id,
                &episode.entry.url,
                episode.entry.media_url.as_deref(),
                &episode.entry.title,
                episode.entry.date.as_deref(),
            );
        }
        let matches: std::collections::BTreeSet<_> = episode
            .identity_keys
            .iter()
            .filter_map(|key| by_key.get(key).copied())
            .collect();
        let index = if let Some(&index) = matches.first() {
            merge_episode(kept[index].as_mut().unwrap(), episode);
            for duplicate in matches.into_iter().skip(1) {
                let other = kept[duplicate].take().unwrap();
                merge_episode(kept[index].as_mut().unwrap(), other);
            }
            index
        } else {
            kept.push(Some(episode));
            kept.len() - 1
        };
        for key in &kept[index].as_ref().unwrap().identity_keys {
            by_key.insert(key.clone(), index);
        }
    }
    kept.into_iter().flatten().collect()
}

pub fn archive_only_episodes(
    result: &ScanResult,
    current: &[u8],
) -> Result<Vec<ArchiveEpisode>, String> {
    let current = parse_document(
        current,
        result.feed_urls.first().map_or("", String::as_str),
        "",
    )?;
    let keys: HashSet<_> = current
        .episodes
        .into_iter()
        .flat_map(|episode| episode.identity_keys)
        .collect();
    Ok(result
        .episodes
        .iter()
        .filter(|episode| episode.identity_keys.is_disjoint(&keys))
        .cloned()
        .collect())
}

/// Parse a Wayback CDX JSON response into snapshot timestamps
/// (newest last). Pure; fixture-tested.
pub fn parse_cdx_timestamps(json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if i == 0 {
            continue; // header row
        }
        if let Some(ts) = row.get(1).and_then(|v| v.as_str())
            && !ts.is_empty()
        {
            out.push(ts.to_string());
        }
    }
    out
}

fn public_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".local")
        || host.ends_with(".localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url.into())
}

fn snapshot_rows(bytes: &[u8], limit: usize) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        return Vec::new();
    };
    let Some(header) = rows.first().and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let Some(ts) = header.iter().position(|v| v.as_str() == Some("timestamp")) else {
        return Vec::new();
    };
    let Some(original) = header.iter().position(|v| v.as_str() == Some("original")) else {
        return Vec::new();
    };
    let mut out = std::collections::BTreeSet::new();
    for row in rows.iter().skip(1) {
        if let (Some(ts), Some(original)) = (
            row.get(ts).and_then(|v| v.as_str()),
            row.get(original).and_then(|v| v.as_str()),
        ) && !ts.is_empty()
            && ts.len() <= 14
            && ts.bytes().all(|b| b.is_ascii_digit())
            && let Some(original) = public_url(original)
        {
            out.insert((ts.to_string(), original));
        }
    }
    out.into_iter().rev().take(limit).collect()
}

fn lineage(document: &[u8], base: &str) -> Vec<String> {
    let html = String::from_utf8_lossy(document);
    let mut urls = Vec::new();
    if let Ok(re) =
        regex::Regex::new(r"(?is)<(?:itunes:)?new-feed-url\b[^>]*>(.*?)</(?:itunes:)?new-feed-url>")
    {
        for capture in re.captures_iter(&html) {
            let value = crate::extract::leaf_text(capture[1].trim());
            if let Some(url) = url::Url::parse(base)
                .ok()
                .and_then(|base| base.join(&value).ok())
                .and_then(|url| public_url(url.as_str()))
            {
                urls.push(url);
            }
        }
    }
    if let Ok(base) = url::Url::parse(base) {
        for span in crate::extract::find_elements(&html, Some("link"), &|tag| {
            crate::extract::attr_value(tag, "rel").is_some_and(|r| r.eq_ignore_ascii_case("self"))
        }) {
            if let Some(href) =
                crate::extract::attr_value(&html[span.start..span.inner_start], "href")
                && let Ok(url) = base.join(&href)
                && let Some(url) = public_url(url.as_str())
            {
                urls.push(url);
            }
        }
    }
    urls
}

pub fn scan_with_fetch(
    seeds: &[String],
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
    mut fetch: impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
) -> Result<ScanResult, String> {
    use std::collections::VecDeque;
    if seeds.is_empty() {
        return Err("Enter a podcast feed URL".into());
    }
    let mut pending = VecDeque::new();
    let mut seen_urls = HashSet::new();
    for seed in seeds {
        let url =
            public_url(seed).ok_or("Podcast archive requires a public HTTP or HTTPS feed URL")?;
        if seen_urls.len() < 12 && seen_urls.insert(canonical_url(&url, false)) {
            pending.push_back(url);
        }
    }
    let mut result = ScanResult::default();
    let mut remaining = options.max_snapshots.clamp(1, 5000);
    let emit = |stage, result: &ScanResult, progress: &mut dyn FnMut(ScanProgress)| {
        progress(ScanProgress {
            stage,
            snapshots_found: result.snapshots_found,
            snapshots_loaded: result.snapshots_loaded,
        });
    };
    let mut documents = Vec::new();
    while let Some(feed_url) = pending.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        result.feed_urls.push(feed_url.clone());
        emit("current", &result, &mut progress);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match fetch(&feed_url).and_then(|(bytes, final_url)| {
            let final_url = public_url(&final_url).unwrap_or_else(|| feed_url.clone());
            parse_document(&bytes, &final_url, "").map(|document| (document, final_url))
        }) {
            Ok((mut document, final_url)) => {
                document.lineage.insert(0, final_url);
                for candidate in &document.lineage {
                    if seen_urls.len() < 12 && seen_urls.insert(canonical_url(candidate, false)) {
                        pending.push_back(candidate.clone());
                    }
                }
                documents.push(document);
            }
            Err(error) => result.warnings.push(error),
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if remaining == 0 {
            result.truncated = true;
            continue;
        }
        emit("index", &result, &mut progress);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut index = url::Url::parse("https://web.archive.org/cdx/search/cdx").unwrap();
        index
            .query_pairs_mut()
            .append_pair("url", &feed_url)
            .append_pair("output", "json")
            .append_pair("filter", "statuscode:200")
            .append_pair("collapse", "digest")
            .append_pair("filter", "!mimetype:warc/revisit")
            .append_pair("limit", &(remaining + 1).to_string());
        match fetch(index.as_str()) {
            Ok((bytes, _)) => {
                let mut snapshots = snapshot_rows(&bytes, remaining + 1);
                result.truncated |= snapshots.len() > remaining;
                snapshots.truncate(remaining);
                result.snapshots_found += snapshots.len();
                remaining -= snapshots.len();
                for (timestamp, original) in snapshots {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    emit("snapshot", &result, &mut progress);
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let snapshot = format!("https://web.archive.org/web/{timestamp}id_/{original}");
                    match fetch(&snapshot)
                        .and_then(|(bytes, _)| parse_document(&bytes, &original, &timestamp))
                    {
                        Ok(document) => {
                            for candidate in &document.lineage {
                                if seen_urls.len() < 12
                                    && seen_urls.insert(canonical_url(candidate, false))
                                {
                                    pending.push_back(candidate.clone());
                                }
                            }
                            result.snapshots_loaded += 1;
                            documents.push(document);
                        }
                        Err(error) => {
                            result.failed_snapshots += 1;
                            result.warnings.push(error);
                        }
                    }
                }
            }
            Err(error) => result.warnings.push(error),
        }
    }
    // All current documents precede snapshots, including migrated feeds.
    let mut episodes = Vec::new();
    for document in documents {
        if result.feed_title.is_empty() {
            result.feed_title = document.title.clone();
        }
        result.readable_documents += 1;
        #[cfg(test)]
        result.documents.push(ParsedFeed {
            title: document.title.clone(),
            entries: document
                .episodes
                .iter()
                .map(|episode| episode.entry.clone())
                .collect(),
        });
        episodes.extend(document.episodes);
    }
    episodes.sort_by_key(|episode| !episode.snapshot_timestamp.is_empty());
    result.episodes = deduplicate_episodes(episodes);
    if !options.include_without_media {
        result.episodes.retain(|episode| {
            episode
                .entry
                .media_url
                .as_deref()
                .is_some_and(|url| !url.is_empty())
        });
    }
    result.episodes.sort_by(|a, b| {
        b.entry.date.cmp(&a.entry.date).then_with(|| {
            b.entry
                .title
                .to_lowercase()
                .cmp(&a.entry.title.to_lowercase())
        })
    });
    result.canceled = cancel.load(Ordering::Relaxed);
    emit("complete", &result, &mut progress);
    Ok(result)
}

#[cfg(test)]
fn scan(
    seed: &str,
    limit: usize,
    fetch: impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
) -> Result<Vec<ParsedFeed>, String> {
    let result = scan_with_fetch(
        &[seed.into()],
        ScanOptions {
            max_snapshots: limit,
            include_without_media: true,
        },
        &AtomicBool::new(false),
        |_| {},
        fetch,
    )?;
    if result.readable_documents == 0 {
        return Err("No readable live or archived podcast feed was found".into());
    }
    let mut seen = HashSet::new();
    Ok(result
        .documents
        .into_iter()
        .map(|mut document| {
            document.entries.retain(|entry| {
                let keys = identity_keys(
                    &entry.id,
                    &entry.url,
                    entry.media_url.as_deref(),
                    &entry.title,
                    entry.date.as_deref(),
                );
                let fresh = keys.is_disjoint(&seen);
                seen.extend(keys);
                fresh
            });
            document
        })
        .collect())
}

/// Rescan the live document, archived snapshots and migrated feed URLs.
/// Returns the number of newly stored articles.
pub fn rescan_archive(db: &Db, feed: &Feed) -> Result<usize, String> {
    let result = scan_with_fetch(
        std::slice::from_ref(&feed.url),
        ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
        |url| {
            let mut response = crate::http::agent()
                .get(url)
                .call()
                .map_err(|e| e.to_string())?;
            let final_url = response.get_uri().to_string();
            let bytes = response
                .body_mut()
                .with_config()
                .limit(10 * 1024 * 1024)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            Ok((bytes, final_url))
        },
    )?;
    if result.readable_documents == 0 {
        return Err(result
            .warnings
            .into_iter()
            .next()
            .unwrap_or_else(|| "No readable live or archived podcast feed was found".into()));
    }
    crate::refresh::store_parsed(
        db,
        feed,
        &ParsedFeed {
            title: result.feed_title,
            entries: result
                .episodes
                .into_iter()
                .map(|episode| episode.entry)
                .collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(id: &str, extra: &str) -> Vec<u8> {
        format!("<rss version='2.0' xmlns:itunes='http://www.itunes.com/dtds/podcast-1.0.dtd'><channel><title>Show</title>{extra}<item><guid>{id}</guid><title>{id}</title><enclosure url='https://cdn.example/{id}.mp3' type='audio/mpeg' length='10'/></item></channel></rss>").into_bytes()
    }

    #[test]
    fn archive_reads_every_snapshot_and_survives_dead_live_feed() {
        let mut requested = Vec::new();
        let feeds = scan("https://example.com/rss", 5000, |url| {
            requested.push(url.to_owned());
            if url.contains("/cdx/") {
                Ok((br#"[["original","timestamp"],["https://example.com/rss","20260101"],["https://example.com/rss","20250101"],["https://example.com/rss","20260101"]]"#.to_vec(), url.into()))
            } else if url.contains("20260101id_") { Ok((document("new", ""), url.into())) }
            else if url.contains("20250101id_") { Ok((document("old", ""), url.into())) }
            else { Err("Publisher has closed".into()) }
        }).unwrap();
        let ids: Vec<_> = feeds
            .iter()
            .flat_map(|f| f.entries.iter().map(|e| e.id.as_str()))
            .collect();
        assert_eq!(ids, ["new", "old"]);
        assert_eq!(requested.iter().filter(|u| u.contains("id_/")).count(), 2);
    }

    #[test]
    fn archive_follows_lineage_and_current_metadata_wins() {
        let feeds = scan("https://example.com/old", 2, |url| {
            if url.contains("/cdx/") {
                Ok((b"[]".to_vec(), url.into()))
            } else if url.ends_with("/old") {
                Ok((
                    document(
                        "same",
                        "<itunes:new-feed-url>https://example.com/new</itunes:new-feed-url>",
                    ),
                    url.into(),
                ))
            } else {
                Ok((document("same", ""), url.into()))
            }
        })
        .unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds.iter().map(|f| f.entries.len()).sum::<usize>(), 1);
    }
}
