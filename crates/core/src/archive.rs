//! Podcast archive recovery (port of `core/podcast_archive.py`).
//!
//! Python's archive follows feed lineage through Wayback snapshots
//! to recover episodes the live feed document no longer carries.
//! Current documents take precedence over older snapshots. Feed migrations
//! are followed with bounded lineage, snapshot count, and document sizes.
//! Fetches retry transient Archive.org failures with Python's bounded
//! backoff, scans report progress and honour cancellation, and archive
//! walks process-wide are serialized through one background job queue
//! (Python's `AUTOMATIC_SCAN_LOCK` + `enqueue_archive_job`). The
//! scan-and-persist orchestrations for local feeds (recovered episodes
//! fold into the ordinary articles table) and hosted feeds (recovered
//! episodes land in the account-scoped sidecar) live at the bottom of
//! this module, over the state store in `archive_store`.

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
    /// The hosted sidecar episode key (Python
    /// `_podcast_archive_episode_key`, duplicated identically in the
    /// hosted mixin and Miniflux): SHA-256 of the sorted identity
    /// keys joined by newlines, or of the
    /// `guid|media|page|title|published` tuple when the episode
    /// carries no identity keys.
    pub fn key(&self) -> String {
        use sha2::Digest;
        let seed = if self.identity_keys.is_empty() {
            format!(
                "{}|{}|{}|{}|{}",
                self.entry.id,
                self.entry.media_url.as_deref().unwrap_or(""),
                self.entry.url,
                self.entry.title,
                self.entry.date.as_deref().unwrap_or("")
            )
        } else {
            let mut keys: Vec<&str> = self.identity_keys.iter().map(String::as_str).collect();
            keys.sort_unstable();
            keys.join("\n")
        };
        sha2::Sha256::digest(seed.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Python `ArchiveEpisode.stable_id`: the id assigned to a
    /// recovered entry whose feed gave it no GUID — the media URL,
    /// then the page URL, seed the digest.
    pub fn stable_id(&self) -> String {
        use sha2::Digest;
        let seed = if !self.entry.id.is_empty() {
            self.entry.id.clone()
        } else if let Some(media) = self.entry.media_url.as_deref().filter(|m| !m.is_empty()) {
            media.to_string()
        } else if !self.entry.url.is_empty() {
            self.entry.url.clone()
        } else {
            format!(
                "{}|{}|",
                self.entry.title,
                self.entry.date.as_deref().unwrap_or("")
            )
        };
        let digest: String = sha2::Sha256::digest(seed.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("podcast-archive:{digest}")
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveDocument {
    pub title: String,
    pub episodes: Vec<ArchiveEpisode>,
    pub lineage: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub max_snapshots: usize,
    pub include_without_media: bool,
    /// The already-fetched live document for the first seed feed
    /// (Python's `current_document`): refresh pipelines that just
    /// downloaded the feed hand it over instead of paying a second
    /// request. Consumed by the first feed the scan processes.
    pub current_document: Option<Vec<u8>>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_snapshots: 5000,
            include_without_media: false,
            current_document: None,
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

/// One progress report (Python's progress dict): the stage plus
/// running counts. `episodes_found` counts episodes whose identity
/// keys are all new so far — a deduplicated estimate that matches
/// the final count unless a later episode bridges two earlier
/// ones into a single merged episode.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub stage: &'static str,
    pub feed_urls: usize,
    pub snapshots_found: usize,
    pub snapshots_loaded: usize,
    pub episodes_found: usize,
}

/// True when a parsed feed carries at least one playable
/// enclosure (Python `feed_has_podcast_media`) — the gate that
/// decides whether a feed pays for a Wayback walk at all.
pub fn feed_has_podcast_media(entries: &[ParsedEntry]) -> bool {
    entries.iter().any(|entry| {
        entry
            .media_url
            .as_deref()
            .is_some_and(|url| !url.is_empty())
    })
}

/// The error a finished scan records when it loaded nothing
/// (Python `_podcast_archive_result_error`, shared by the local
/// provider and both hosted implementations): the first warning,
/// or the generic snapshot failure when snapshots existed but
/// none loaded. Empty when the scan loaded at least one snapshot
/// or found nothing to load.
pub fn result_error(result: &ScanResult) -> String {
    if result.snapshots_loaded == 0 {
        if let Some(warning) = result.warnings.first() {
            return warning.clone();
        }
        if result.snapshots_found > 0 {
            return "The Wayback Machine snapshots could not be loaded.".into();
        }
    }
    String::new()
}

/// Whether a fetch failure looks like one of Archive.org's
/// transient responses (HTTP 429/5xx, timeouts, connection
/// trouble) that Python's `_fetch_with_retry` retries.
fn is_transient_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    ["429", "500", "502", "503", "504"]
        .iter()
        .any(|code| lower.contains(code))
        || [
            "timed out",
            "timeout",
            "connection",
            "temporar",
            "reset by peer",
            "broken pipe",
            "unexpected eof",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

fn sleep_cancelable(cancel: &AtomicBool, seconds: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    while std::time::Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Python `_fetch_with_retry`: three attempts, doubling backoff
/// (1 s, 2 s), only for transient failures; cancellation between
/// attempts reports the scan as canceled.
fn fetch_with_retry(
    fetch: &mut impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
    url: &str,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, String), String> {
    let mut last_error = String::new();
    for attempt in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            return Err("Podcast archive scan canceled.".into());
        }
        match fetch(url) {
            Ok(value) => return Ok(value),
            Err(error) => {
                let transient = is_transient_error(&error);
                last_error = error;
                if !transient || attempt == 2 {
                    break;
                }
                sleep_cancelable(cancel, f64::from(1u32 << attempt));
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Podcast archive scan canceled.".into());
    }
    Err(last_error)
}

/// Process-wide serialization for archive walks (Python's
/// `AUTOMATIC_SCAN_LOCK`): a refresh may have dozens of feed
/// workers, but archive.org should see only one history walk at
/// a time.
static SCAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn automatic_scan_lock() -> &'static std::sync::Mutex<()> {
    &SCAN_LOCK
}

/// Enqueue a job on the single background archive worker
/// (Python's `enqueue_archive_job`): jobs run serially on one
/// daemon thread, so queueing never delays a feed refresh, and a
/// failing job is logged by the caller's own error handling
/// without killing the worker.
pub fn enqueue_archive_job(job: impl FnOnce() + Send + 'static) {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<Box<dyn FnOnce() + Send>>> =
        std::sync::OnceLock::new();
    let sender = SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let _ = std::thread::Builder::new()
            .name("PodcastArchiveWorker".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            });
        tx
    });
    let _ = sender.send(Box::new(job));
}

/// The archive scanner's network fetch (the shared HTTP agent,
/// bounded to Python's 10 MiB document cap). Returns the body
/// and the final URL after redirects.
pub fn http_fetch(url: &str) -> Result<(Vec<u8>, String), String> {
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

/// Python `_episode_identity_keys`. An episode earns a GUID key
/// (canonicalized only when the GUID is a URL, as Python does), a
/// media key and a page key (both canonical, so rotating signed
/// media tokens still match), and a title key: `title-date` when a
/// real publication day exists, else `title-duration` — Python's
/// fallback for undated episodes. Keys whose value part is empty
/// are dropped, as in Python. `duration` is Python's
/// itunes:duration; the Rust feed parser does not retain it yet,
/// so callers pass None and the duration branch stays inert until
/// the parser carries it.
pub fn episode_identity_keys(
    id: &str,
    page: &str,
    media: Option<&str>,
    title: &str,
    date: Option<&str>,
    duration: Option<&str>,
) -> HashSet<String> {
    let mut keys = HashSet::new();
    let guid = id.trim();
    if !guid.is_empty() {
        let guid = if guid.contains("://") {
            canonical_url(guid, false)
        } else {
            guid.to_string()
        };
        keys.insert(format!("guid:{}", guid.to_lowercase()));
    }
    if !page.trim().is_empty() {
        keys.insert(format!("page:{}", canonical_url(page, false)));
    }
    if let Some(media) = media.filter(|media| !media.trim().is_empty()) {
        keys.insert(format!("media:{}", canonical_url(media, true)));
    }
    let title = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let day = date
        .and_then(|date| date.get(..10))
        .filter(|day| !day.starts_with("0001-"));
    if !title.is_empty() {
        if let Some(day) = day {
            keys.insert(format!("title-date:{title}|{day}"));
        } else if let Some(duration) = duration.filter(|d| !d.trim().is_empty()) {
            keys.insert(format!("title-duration:{title}|{}", duration.trim()));
        }
    }
    keys.retain(|key| {
        key.rsplit(':')
            .next()
            .is_some_and(|value| !value.is_empty())
    });
    keys
}

/// Identity keys for one parsed feed entry. feed-rs invents a
/// random UUID id for entries with no guid, and feeds.rs mirrors
/// it into the entry URL when the entry also has no link (the
/// archive base join then turns it into "<feed dir>/<uuid>").
/// Python's feedparser leaves both empty, so the invented values
/// must not contribute guid/page identity: the random id changes
/// on every parse, which would make the episode's key unstable
/// and reconciliation across snapshots impossible. (A real
/// UUID-shaped guid WITH a link keeps its keys; the only loss is
/// a UUID guid with no link, no media and no date, which
/// feedparser would key by guid alone.)
fn entry_identity_keys(entry: &ParsedEntry) -> HashSet<String> {
    let id = entry.id.as_str();
    let url = entry.url.as_str();
    let synthesized =
        uuid::Uuid::parse_str(id).is_ok() && (url == id || url.ends_with(&format!("/{id}")));
    let (id, url) = if synthesized { ("", "") } else { (id, url) };
    episode_identity_keys(
        id,
        url,
        entry.media_url.as_deref(),
        &entry.title,
        entry.date.as_deref(),
        None,
    )
}

pub fn article_identity_keys(article: &crate::Article) -> HashSet<String> {
    episode_identity_keys(
        &article.id,
        &article.url,
        article.media_url.as_deref(),
        &article.title,
        article.date.as_deref(),
        None,
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
        .filter_map(|mut entry| {
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
            let identity_keys = entry_identity_keys(&entry);
            // Python keeps only episodes that carry at least one
            // identity key; a keyless entry can never be reconciled
            // against another snapshot and is dropped.
            if identity_keys.is_empty() {
                return None;
            }
            Some(ArchiveEpisode {
                entry,
                source_feed_url: source_url.into(),
                snapshot_timestamp: timestamp.into(),
                identity_keys,
            })
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
    preferred.identity_keys.extend(entry_identity_keys(current));
}

pub fn deduplicate_episodes(
    episodes: impl IntoIterator<Item = ArchiveEpisode>,
) -> Vec<ArchiveEpisode> {
    let mut kept: Vec<Option<ArchiveEpisode>> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for mut episode in episodes {
        if episode.identity_keys.is_empty() {
            episode.identity_keys = entry_identity_keys(&episode.entry);
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
    let mut current_document = options.current_document;
    let mut seen_episode_keys: HashSet<String> = HashSet::new();
    let mut episodes_found = 0usize;
    let emit = |stage,
                result: &ScanResult,
                episodes_found: usize,
                progress: &mut dyn FnMut(ScanProgress)| {
        progress(ScanProgress {
            stage,
            feed_urls: result.feed_urls.len(),
            snapshots_found: result.snapshots_found,
            snapshots_loaded: result.snapshots_loaded,
            episodes_found,
        });
    };
    // Fold one parsed document into the running progress estimate.
    macro_rules! note_document {
        ($document:expr) => {
            for episode in &$document.episodes {
                if episode.identity_keys.is_disjoint(&seen_episode_keys) {
                    episodes_found += 1;
                }
                seen_episode_keys.extend(episode.identity_keys.iter().cloned());
            }
        };
    }
    let mut documents = Vec::new();
    while let Some(feed_url) = pending.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        result.feed_urls.push(feed_url.clone());
        emit("current", &result, episodes_found, &mut progress);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let live = if let Some(bytes) = current_document.take() {
            parse_document(&bytes, &feed_url, "").map(|document| (document, feed_url.clone()))
        } else {
            fetch_with_retry(&mut fetch, &feed_url, cancel).and_then(|(bytes, final_url)| {
                let final_url = public_url(&final_url).unwrap_or_else(|| feed_url.clone());
                parse_document(&bytes, &final_url, "").map(|document| (document, final_url))
            })
        };
        match live {
            Ok((mut document, final_url)) => {
                document.lineage.insert(0, final_url);
                for candidate in &document.lineage {
                    if seen_urls.len() < 12 && seen_urls.insert(canonical_url(candidate, false)) {
                        pending.push_back(candidate.clone());
                    }
                }
                note_document!(document);
                documents.push(document);
            }
            Err(error) => {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                result.warnings.push(error);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if remaining == 0 {
            result.truncated = true;
            continue;
        }
        emit("index", &result, episodes_found, &mut progress);
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
        match fetch_with_retry(&mut fetch, index.as_str(), cancel) {
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
                    emit("snapshot", &result, episodes_found, &mut progress);
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let snapshot = format!("https://web.archive.org/web/{timestamp}id_/{original}");
                    match fetch_with_retry(&mut fetch, &snapshot, cancel)
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
                            note_document!(document);
                            documents.push(document);
                        }
                        Err(error) => {
                            if cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            result.failed_snapshots += 1;
                            result.warnings.push(error);
                        }
                    }
                }
            }
            Err(error) => {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                result.warnings.push(error);
            }
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
    emit("complete", &result, result.episodes.len(), &mut progress);
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
            current_document: None,
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
                let keys = episode_identity_keys(
                    &entry.id,
                    &entry.url,
                    entry.media_url.as_deref(),
                    &entry.title,
                    entry.date.as_deref(),
                    None,
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
/// Returns the number of newly stored articles. This is the main
/// window's entry point: it stores the scan's full episode set
/// (current + recovered) through the ordinary store path. Callers
/// that need the archive state store updated alongside (the
/// Podcast Archive dialog, the background queues) use
/// [`perform_local_scan`] with the database path instead.
pub fn rescan_archive(db: &Db, feed: &Feed) -> Result<usize, String> {
    let result = scan_with_fetch(
        std::slice::from_ref(&feed.url),
        ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
        http_fetch,
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

/// How a local scan's state-store claim is taken (Python's three
/// call shapes): a user-requested scan claims with force, an
/// automatic scan claims only when due, and a queued job whose
/// claim was taken before enqueueing runs already holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Force,
    Due,
    AlreadyHeld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalScanKind {
    /// The scan ran; recovered episodes were stored.
    Stored,
    /// The current document carries no podcast media, so no scan
    /// was claimed or run (Python only scans media feeds).
    NotPodcast,
    /// The state store says the feed is not due yet.
    NotDue,
    /// The scan was canceled; the claim was reset so the feed is
    /// due again immediately, exactly as Python resets it.
    Canceled,
}

#[derive(Debug, Clone)]
pub struct LocalScanOutcome {
    pub kind: LocalScanKind,
    /// Archive-only episodes newly stored as articles.
    pub recovered: usize,
    pub snapshots_loaded: usize,
    /// Total episodes the scan reconciled (Python records this
    /// as the state's episode_count).
    pub episodes: usize,
    /// The recorded scan error (Python's result error), empty on
    /// a clean scan. Recovered episodes are still stored when
    /// this is set, matching Python's deferred completion.
    pub error: String,
}

impl LocalScanOutcome {
    fn simple(kind: LocalScanKind) -> Self {
        Self {
            kind,
            recovered: 0,
            snapshots_loaded: 0,
            episodes: 0,
            error: String::new(),
        }
    }
}

/// The local scan-and-store orchestration (Python
/// `LocalProvider`'s archive flow, with its two-pass pending
/// result dance collapsed into one pass: `store_parsed` applies
/// the same tombstone and filter-rule pipeline the second
/// refresh applied in Python). The current document is fetched
/// (or supplied by a refresh that just downloaded it); only a
/// media-bearing document claims and scans. State transitions
/// mirror Python: canceled → claim reset, otherwise finish with
/// the snapshot/episode counts or the result error.
#[allow(clippy::too_many_arguments)]
pub fn perform_local_scan_with_fetch(
    db_path: &std::path::Path,
    feed: &Feed,
    settings: &crate::archive_store::ArchiveSettings,
    claim: Claim,
    current_document: Option<Vec<u8>>,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
    mut fetch: impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
) -> Result<LocalScanOutcome, String> {
    use crate::archive_store as store;
    let (bytes, final_url) = match current_document {
        Some(bytes) => (bytes, feed.url.clone()),
        None => fetch(&feed.url)?,
    };
    let current = parse_document(&bytes, &final_url, "")?;
    if !feed_has_podcast_media(
        &current
            .episodes
            .iter()
            .map(|episode| episode.entry.clone())
            .collect::<Vec<_>>(),
    ) {
        return Ok(LocalScanOutcome::simple(LocalScanKind::NotPodcast));
    }
    let claimed = match claim {
        Claim::AlreadyHeld => true,
        Claim::Force | Claim::Due => store::claim(
            db_path,
            "",
            &feed.id,
            &feed.url,
            settings.local_rescan_seconds(),
            claim == Claim::Force,
            store::now_seconds(),
        )?,
    };
    if !claimed {
        return Ok(LocalScanOutcome::simple(LocalScanKind::NotDue));
    }
    let result = {
        let _guard = automatic_scan_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        scan_with_fetch(
            std::slice::from_ref(&feed.url),
            ScanOptions {
                max_snapshots: settings.max_snapshots,
                include_without_media: true,
                current_document: Some(bytes.clone()),
            },
            cancel,
            progress,
            &mut fetch,
        )?
    };
    if result.canceled {
        let _ = store::reset(db_path, "", &feed.id);
        return Ok(LocalScanOutcome::simple(LocalScanKind::Canceled));
    }
    let recovered_episodes = archive_only_episodes(&result, &bytes)?;
    let db = Db::open(db_path).map_err(|e| e.to_string())?;
    let recovered = crate::refresh::store_parsed(
        &db,
        feed,
        &ParsedFeed {
            title: result.feed_title.clone(),
            entries: recovered_episodes
                .into_iter()
                .map(|episode| episode.entry)
                .collect(),
        },
    )?;
    let error = result_error(&result);
    store::finish(
        db_path,
        "",
        &feed.id,
        result.snapshots_loaded,
        result.episodes.len(),
        &error,
        store::now_seconds(),
    )?;
    Ok(LocalScanOutcome {
        kind: LocalScanKind::Stored,
        recovered,
        snapshots_loaded: result.snapshots_loaded,
        episodes: result.episodes.len(),
        error,
    })
}

/// [`perform_local_scan_with_fetch`] over the real network.
pub fn perform_local_scan(
    db_path: &std::path::Path,
    feed: &Feed,
    settings: &crate::archive_store::ArchiveSettings,
    claim: Claim,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
) -> Result<LocalScanOutcome, String> {
    perform_local_scan_with_fetch(
        db_path, feed, settings, claim, None, cancel, progress, http_fetch,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedScanKind {
    /// Recovered episodes replaced the feed's sidecar rows.
    Stored,
    /// The current document carries no podcast media: Python
    /// clears the sidecar rows and finishes the scan complete.
    Cleared,
    /// Canceled; the claim was reset so the feed is due again.
    Canceled,
    /// The scan recorded an error; earlier sidecar rows survive.
    Failed,
}

#[derive(Debug, Clone)]
pub struct HostedScanOutcome {
    pub kind: HostedScanKind,
    /// Archive-only episodes now held in the sidecar.
    pub recovered: usize,
    pub error: String,
}

/// The hosted scan-and-store orchestration (Python
/// `HostedPodcastArchiveMixin._run_podcast_archive_scan`, also
/// Miniflux's copy). Assumes the caller already holds the claim
/// (the queue job claims inside the job; the manual path resets
/// and force-claims — see [`refresh_hosted_archive`]). All state
/// and entries are written under `scope` in the account's own
/// database file, never the local store or another account's.
#[allow(clippy::too_many_arguments)]
pub fn run_hosted_scan_with_fetch(
    db_path: &std::path::Path,
    scope: &str,
    feed_id: &str,
    feed_url: &str,
    settings: &crate::archive_store::ArchiveSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
    mut fetch: impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
) -> HostedScanOutcome {
    use crate::archive_store as store;
    let failed = |error: String| HostedScanOutcome {
        kind: HostedScanKind::Failed,
        recovered: 0,
        error,
    };
    let (bytes, final_url) = match fetch(feed_url) {
        Ok(value) => value,
        Err(error) => {
            let _ = store::finish(db_path, scope, feed_id, 0, 0, &error, store::now_seconds());
            return failed(error);
        }
    };
    let current = match parse_document(&bytes, &final_url, "") {
        Ok(document) => document,
        Err(error) => {
            let _ = store::finish(db_path, scope, feed_id, 0, 0, &error, store::now_seconds());
            return failed(error);
        }
    };
    if !current.episodes.iter().any(|episode| {
        episode
            .entry
            .media_url
            .as_deref()
            .is_some_and(|m| !m.is_empty())
    }) {
        let _ = store::replace_entries(db_path, scope, feed_id, &[]);
        let _ = store::finish(db_path, scope, feed_id, 0, 0, "", store::now_seconds());
        return HostedScanOutcome {
            kind: HostedScanKind::Cleared,
            recovered: 0,
            error: String::new(),
        };
    }
    let scan = {
        let _guard = automatic_scan_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        scan_with_fetch(
            &[feed_url.to_string()],
            ScanOptions {
                max_snapshots: settings.max_snapshots,
                include_without_media: true,
                current_document: Some(bytes.clone()),
            },
            cancel,
            progress,
            &mut fetch,
        )
    };
    let result = match scan {
        Ok(result) => result,
        Err(error) => {
            let _ = store::finish(db_path, scope, feed_id, 0, 0, &error, store::now_seconds());
            return failed(error);
        }
    };
    if result.canceled {
        let _ = store::reset(db_path, scope, feed_id);
        return HostedScanOutcome {
            kind: HostedScanKind::Canceled,
            recovered: 0,
            error: String::new(),
        };
    }
    let error = result_error(&result);
    if !error.is_empty() {
        // A transient archive outage must not erase history an
        // earlier successful scan recovered.
        let _ = store::finish(
            db_path,
            scope,
            feed_id,
            result.snapshots_loaded,
            result.episodes.len(),
            &error,
            store::now_seconds(),
        );
        return failed(error);
    }
    let recovered = match archive_only_episodes(&result, &bytes) {
        Ok(episodes) => episodes
            .into_iter()
            .filter(|episode| {
                episode
                    .entry
                    .media_url
                    .as_deref()
                    .is_some_and(|m| !m.is_empty())
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            let _ = store::finish(db_path, scope, feed_id, 0, 0, &error, store::now_seconds());
            return failed(error);
        }
    };
    let count = recovered.len();
    if let Err(error) = store::replace_entries(db_path, scope, feed_id, &recovered) {
        let _ = store::finish(db_path, scope, feed_id, 0, 0, &error, store::now_seconds());
        return failed(error);
    }
    let _ = store::finish(
        db_path,
        scope,
        feed_id,
        result.snapshots_loaded,
        result.episodes.len(),
        "",
        store::now_seconds(),
    );
    HostedScanOutcome {
        kind: HostedScanKind::Stored,
        recovered: count,
        error: String::new(),
    }
}

/// [`run_hosted_scan_with_fetch`] over the real network.
#[allow(clippy::too_many_arguments)]
pub fn run_hosted_scan(
    db_path: &std::path::Path,
    scope: &str,
    feed_id: &str,
    feed_url: &str,
    settings: &crate::archive_store::ArchiveSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
) -> HostedScanOutcome {
    run_hosted_scan_with_fetch(
        db_path, scope, feed_id, feed_url, settings, cancel, progress, http_fetch,
    )
}

/// The manual hosted rescan (Python `refresh_podcast_archive`):
/// reset the state, force-claim, then run the scan. Returns
/// false when the scan did not complete (canceled or failed).
#[allow(clippy::too_many_arguments)]
pub fn refresh_hosted_archive(
    db_path: &std::path::Path,
    scope: &str,
    feed_id: &str,
    feed_url: &str,
    settings: &crate::archive_store::ArchiveSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
) -> bool {
    use crate::archive_store as store;
    if feed_id.is_empty() || feed_url.is_empty() {
        return false;
    }
    if store::reset(db_path, scope, feed_id).is_err() {
        return false;
    }
    match store::claim(
        db_path,
        scope,
        feed_id,
        feed_url,
        settings.hosted_rescan_seconds(),
        true,
        store::now_seconds(),
    ) {
        Ok(true) => {}
        _ => return false,
    }
    matches!(
        run_hosted_scan(
            db_path, scope, feed_id, feed_url, settings, cancel, progress
        )
        .kind,
        HostedScanKind::Stored | HostedScanKind::Cleared
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
