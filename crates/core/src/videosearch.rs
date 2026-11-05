//! Cross-site video search (Python `discovery.search_ytdlp_site`
//! + the site set behind the YtdlpGlobalSearchDialog).
//!
//! The site list derives from the installed yt-dlp at runtime
//! (Audit 8 F06): Python reads its extractor classes in-process
//! (`_SEARCH_KEY`, `IE_NAME`, `IE_DESC`); the port wraps the
//! yt-dlp executable, whose only registry surface is
//! `--list-extractors` — extractor NAMES, no search keys. The
//! bridge below enumerates those names and maps each search
//! extractor through a verified key table (keys confirmed
//! against the installed yt-dlp 2026.08.19; a name with no
//! verified key cannot be query-searched and is not offered).
//! Python's derivation rules are ported exactly: label
//! overrides, the redundant-site drop, adult keyword gating,
//! non-date preference with alias retention, sort by label,
//! the Mixcloud extra, and the curated adult templates
//! appended after the sort.
//!
//! Each site search is one yt-dlp flat-playlist dump through
//! the shared owned-subprocess runner, so a hung extractor
//! cannot stall the dialog.

use serde_json::Value;

/// One searchable site.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchSite {
    pub id: String,
    pub label: String,
    /// yt-dlp query-search key (e.g. "ytsearch"), when the site
    /// is searched by query.
    pub search_key: Option<String>,
    /// Search-results page template with a `{query}` slot, for
    /// sites yt-dlp cannot query-search.
    pub url_template: Option<String>,
    /// Searched through a dedicated provider API instead.
    pub provider: Option<String>,
    pub adult: bool,
    /// Search keys that lost the per-site dedupe to
    /// `search_key` (Python `search_key_aliases`).
    pub search_key_aliases: Vec<String>,
}

/// Python `_YTDLP_SEARCH_SITE_LABEL_OVERRIDES`, keyed by
/// search key.
fn label_override(search_key: &str) -> Option<&'static str> {
    Some(match search_key {
        "ytsearch" => "YouTube",
        "bilisearch" => "Bilibili",
        "nicosearch" | "nicosearchdate" => "Niconico",
        "scsearch" => "SoundCloud",
        "rkfnsearch" => "Rokfin",
        "prxseries" => "PRX Series",
        "prxstories" => "PRX Stories",
        "gvsearch" => "Google Video",
        "yvsearch" => "Yahoo Video",
        "netsearch" => "Netverse",
        _ => return None,
    })
}

/// Python `_YTDLP_ADULT_KEYWORDS`.
const ADULT_KEYWORDS: &[&str] = &[
    "adult",
    "porn",
    "sex",
    "erotic",
    "xhamster",
    "xvideos",
    "xnxx",
    "redtube",
    "youporn",
    "tube8",
    "spankbang",
    "hentai",
];

/// Python `_REDUNDANT_SEARCH_SITE_IDS`.
const REDUNDANT_SITE_IDS: &[&str] = &["gvsearch", "yvsearch"];

/// Python `_is_adult_ytdlp_search_extractor`: any adult
/// keyword appearing in the joined extractor text.
fn is_adult_search_text(parts: &[&str]) -> bool {
    let text = parts.join(" ").to_lowercase();
    !text.is_empty() && ADULT_KEYWORDS.iter().any(|tok| text.contains(tok))
}

/// Python `_ytdlp_search_site_id`.
fn site_id_for(search_key: &str, ie_name: &str) -> String {
    let sk = search_key.trim().to_lowercase();
    if sk.is_empty() {
        return String::new();
    }
    if label_override(&sk).is_some() {
        // Keep stable IDs for the overridden keys; the date
        // variant of Niconico folds into the base site.
        if sk == "nicosearchdate" {
            return "nicosearch".to_string();
        }
        return sk;
    }
    let ie_low = ie_name.trim().to_lowercase();
    if let Some(pos) = ie_low.find(":search") {
        let base = ie_low[..pos].trim();
        if !base.is_empty() {
            return base.to_string();
        }
    }
    sk
}

fn title_case(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Python `_ytdlp_search_site_label`.
fn site_label_for(search_key: &str, ie_key: &str, ie_name: &str, ie_desc: &str) -> String {
    let sk = search_key.trim().to_lowercase();
    if let Some(label) = label_override(&sk) {
        return label.to_string();
    }
    let ie_name_text = ie_name.trim();
    if !ie_name_text.is_empty() && ie_name_text.to_lowercase().contains(":search") {
        return title_case(ie_name_text.split(':').next().unwrap_or(""));
    }
    let mut ie_key_text = ie_key.trim().to_string();
    if !ie_key_text.is_empty() {
        let low = ie_key_text.to_lowercase();
        if low.ends_with("searchdate") {
            ie_key_text.truncate(ie_key_text.len() - 10);
        } else if low.ends_with("search") {
            ie_key_text.truncate(ie_key_text.len() - 6);
        }
        if !ie_key_text.is_empty() {
            return ie_key_text;
        }
    }
    let desc_text = ie_desc.trim();
    if !desc_text.is_empty() {
        if desc_text.to_lowercase().ends_with(" search") {
            return desc_text[..desc_text.len() - 7].trim().to_string();
        }
        return desc_text.to_string();
    }
    if sk.is_empty() {
        "Unknown".to_string()
    } else {
        sk
    }
}

/// One parsed `--list-extractors` search name.
struct SearchName {
    /// Lowercased base used for the key-table lookup
    /// ("youtube", "nicovideo", "bilibili", ...).
    base: String,
    ie_name: String,
    ie_key: String,
    is_date: bool,
}

/// Classify one extractor name the way Python's class-name
/// filter does (`*SearchIE` / `*SearchDateIE` / `*Search` /
/// `*SearchDate`): colon names carry `IE_NAME`, CamelCase
/// names are the lazy-extractor `IE_NAME` for classes whose
/// name was never overridden. Search-URL extractors are not
/// query-search sites and are excluded.
fn parse_search_name(name: &str) -> Option<SearchName> {
    let name = name.trim();
    if name.is_empty() || name.contains(":search_url") {
        return None;
    }
    if let Some(pos) = name.find(":search") {
        let after = &name[pos + ":search".len()..];
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        return Some(SearchName {
            base: name[..pos].to_lowercase(),
            ie_name: name.to_string(),
            ie_key: String::new(),
            is_date: after.starts_with(":date"),
        });
    }
    if let Some(base) = name.strip_suffix("SearchDate") {
        return Some(SearchName {
            base: base.to_lowercase(),
            ie_name: String::new(),
            ie_key: name.to_string(),
            is_date: true,
        });
    }
    if let Some(base) = name.strip_suffix("Search") {
        return Some(SearchName {
            base: base.to_lowercase(),
            ie_name: String::new(),
            ie_key: name.to_string(),
            is_date: false,
        });
    }
    None
}

/// The verified search key for an extractor base. Keys were
/// confirmed against the installed yt-dlp: a correct key runs
/// a search, a wrong one fails with "Unsupported url scheme".
/// Bases not listed here have no key the CLI can reveal
/// (their `_SEARCH_KEY` is empty or unverifiable), matching
/// Python's non-empty-`_SEARCH_KEY` requirement.
fn search_key_for_base(base: &str, is_date: bool) -> Option<&'static str> {
    let key = match base {
        "youtube" => "ytsearch",
        "soundcloud" => "scsearch",
        "bilibili" => "bilisearch",
        "nicovideo" => "nicosearch",
        "rokfin" => "rkfnsearch",
        "prxseries" => "prxseries",
        "prxstories" => "prxstories",
        "video.google" => "gvsearch",
        "yahoo" => "yvsearch",
        _ => return None,
    };
    if is_date {
        // The date variant's own key, where one exists, so
        // Python's dedupe preference and alias rules run on
        // the real keys.
        return Some(match key {
            "nicosearch" => "nicosearchdate",
            "ytsearch" => "ytsearchdate",
            _ => key,
        });
    }
    Some(key)
}

/// Python's dedupe preference for two extractors mapping to
/// one site: working first (the CLI cannot report `_WORKING`;
/// every row is treated as working), then non-date keys,
/// then the longer key.
fn key_preference(key: &str) -> (u8, u8, usize) {
    (
        1,
        u8::from(!key.to_lowercase().ends_with("date")),
        key.len(),
    )
}

/// Derive the searchable-site list from extractor names
/// (Python `get_ytdlp_searchable_sites`): dedupe by site id
/// with alias retention, append the Mixcloud extra, sort by
/// (label, id), and append the curated adult template sites
/// after the sort when `include_adult`.
pub fn sites_from_extractor_names(names: &[&str], include_adult: bool) -> Vec<SearchSite> {
    let mut by_id: Vec<SearchSite> = Vec::new();
    for name in names {
        let Some(parsed) = parse_search_name(name) else {
            continue;
        };
        let Some(key) = search_key_for_base(&parsed.base, parsed.is_date) else {
            continue;
        };
        let adult = is_adult_search_text(&[key, &parsed.ie_key, &parsed.ie_name, ""]);
        if adult && !include_adult {
            continue;
        }
        let id = site_id_for(key, &parsed.ie_name);
        if id.is_empty() || REDUNDANT_SITE_IDS.contains(&id.as_str()) {
            continue;
        }
        let row = SearchSite {
            id: id.clone(),
            label: site_label_for(key, &parsed.ie_key, &parsed.ie_name, ""),
            search_key: Some(key.to_string()),
            url_template: None,
            provider: None,
            adult,
            search_key_aliases: Vec::new(),
        };
        match by_id.iter_mut().find(|s| s.id == id) {
            None => by_id.push(row),
            Some(existing) => {
                let existing_key = existing.search_key.clone().unwrap_or_default();
                if key_preference(key) > key_preference(&existing_key) {
                    let mut aliases = vec![existing_key];
                    aliases.extend(existing.search_key_aliases.iter().cloned());
                    aliases.retain(|a| a != key && !a.is_empty());
                    aliases.dedup();
                    let mut row = row;
                    row.search_key_aliases = aliases;
                    *existing = row;
                } else if key != existing_key
                    && !existing.search_key_aliases.iter().any(|a| a == key)
                {
                    existing.search_key_aliases.push(key.to_string());
                }
            }
        }
    }
    // Python `_EXTRA_SEARCH_SITES`: API-backed sites that
    // are not yt-dlp search extractors.
    if !by_id.iter().any(|s| s.id == "mixcloud") {
        by_id.push(SearchSite {
            id: "mixcloud".to_string(),
            label: "Mixcloud".to_string(),
            search_key: None,
            url_template: None,
            provider: Some("mixcloud".to_string()),
            adult: false,
            search_key_aliases: Vec::new(),
        });
    }
    by_id.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase()))
    });
    if include_adult {
        by_id.extend(adult_template_sites());
    }
    by_id
}

/// Python `get_adult_searchable_sites`: curated URL-template
/// sites, offered only when adult search is enabled.
fn adult_template_sites() -> Vec<SearchSite> {
    vec![SearchSite {
        id: "pornhub".to_string(),
        label: "Pornhub".to_string(),
        search_key: None,
        url_template: Some("https://www.pornhub.com/video/search?search={query}".to_string()),
        provider: None,
        adult: true,
        search_key_aliases: Vec::new(),
    }]
}

/// The search-extractor names the installed yt-dlp
/// (2026.08.19) prints, used until the runtime enumeration
/// lands (or when yt-dlp cannot be run at all).
const FALLBACK_EXTRACTOR_NAMES: &[&str] = &[
    "BiliBiliSearch",
    "CiscoLiveSearch",
    "dailymotion:search",
    "GameJoltSearch",
    "mailru:music:search",
    "mellowfan:channel:search",
    "nicovideo:search",
    "nicovideo:search:date",
    "prxseries:search",
    "prxstories:search",
    "RedGifsSearch",
    "rokfin:search",
    "soundcloud:search",
    "video.google:search",
    "vrsquare:search",
    "yahoo:search",
    "youtube:search",
];

static EXTRACTOR_NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
static NAMES_WARMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enumerate the installed yt-dlp's extractor registry on a
/// background thread (Python preloads its registry at
/// startup the same way). Safe to call repeatedly.
pub fn warm_search_sites() {
    if EXTRACTOR_NAMES.get().is_some()
        || NAMES_WARMING.swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    std::thread::spawn(|| {
        let names = run_ytdlp(vec!["--list-extractors".to_string()], 60)
            .map(|out| {
                out.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !names.is_empty() {
            let _ = EXTRACTOR_NAMES.set(names);
        }
        NAMES_WARMING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// The sites offered in Video Search, derived from the
/// installed yt-dlp's extractor registry (Audit 8 F06).
/// Adult sites appear only with `include_adult` (config
/// `enable_adult_search`).
pub fn searchable_sites(include_adult: bool) -> Vec<SearchSite> {
    match EXTRACTOR_NAMES.get() {
        Some(names) => {
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            sites_from_extractor_names(&refs, include_adult)
        }
        None => {
            warm_search_sites();
            sites_from_extractor_names(FALLBACK_EXTRACTOR_NAMES, include_adult)
        }
    }
}

/// One normalized search result (Python's normalized
/// yt-dlp search-result dict, with the duration and owner
/// the detail string is built from kept as fields for the
/// dialog's columns).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoResult {
    pub title: String,
    pub url: String,
    /// Display site label (Python resolves it per result URL
    /// via `_display_site_label_from_result_url`).
    pub site: String,
    pub site_id: String,
    /// "feed" | "user" | "playlist" | "media".
    pub kind: String,
    pub play_count: Option<i64>,
    pub duration: Option<f64>,
    /// Python's owner label (channel/uploader + handle).
    pub uploader: Option<String>,
    /// Python `_build_ytdlp_search_result_detail`.
    pub detail: String,
    /// True when `title` is the friendly URL fallback rather
    /// than a real entry title (Python `_title_is_fallback`).
    pub title_is_fallback: bool,
    /// The site's native feed for this result, when one
    /// exists (Python `native_subscribe_url`).
    pub native_feed_url: String,
    /// The channel/creator page to subscribe through when
    /// there is no native feed (Python
    /// `source_subscribe_url`).
    pub source_subscribe_url: String,
}

impl VideoResult {
    /// The target of Python's Subscribe (best) action: the
    /// native feed when there is one, else the source page.
    pub fn best_subscribe_url(&self) -> Option<&str> {
        if !self.native_feed_url.is_empty() {
            Some(&self.native_feed_url)
        } else if !self.source_subscribe_url.is_empty() {
            Some(&self.source_subscribe_url)
        } else {
            None
        }
    }

    /// Python `search_result_quality_score`: a real title
    /// beats a fallback one, a subscribable row beats one
    /// without targets, then the higher play count wins.
    pub fn quality_score(&self) -> (u8, u8, i64) {
        (
            u8::from(!self.title.trim().is_empty() && !self.title_is_fallback),
            u8::from(!self.native_feed_url.is_empty() || !self.source_subscribe_url.is_empty()),
            self.play_count.unwrap_or(-1),
        )
    }
}

/// Merge one incoming result into `results` by canonical
/// key (Python's dialog merge, Audit 8 F19): a duplicate
/// with a strictly better quality score replaces the stored
/// copy in place, keeping its arrival slot.
pub fn merge_result(results: &mut Vec<VideoResult>, result: VideoResult) {
    let key = canonical_key(&result.url);
    if key.is_empty() {
        results.push(result);
        return;
    }
    match results.iter_mut().find(|r| canonical_key(&r.url) == key) {
        Some(existing) => {
            if result.quality_score() > existing.quality_score() {
                *existing = result;
            }
        }
        None => results.push(result),
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Run yt-dlp and return its stdout. F20: the run goes through
/// the shared owned-subprocess runner, so a timed-out search is
/// KILLED AND REAPED (Python `core/discovery.py` terminates
/// timed-out subprocesses) instead of being abandoned to finish
/// on its own.
fn run_ytdlp(args: Vec<String>, timeout_s: u64) -> Result<String, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let run =
        crate::dependency::run_tool(&exe, &arg_refs, std::time::Duration::from_secs(timeout_s))
            .map_err(|e| {
                if e.contains("timed out") {
                    "yt-dlp search timed out".to_string()
                } else {
                    format!("yt-dlp: {e}")
                }
            })?;
    if run.success {
        Ok(run.stdout)
    } else {
        Err(format!(
            "yt-dlp: {}",
            run.stderr.lines().last().unwrap_or("failed").trim()
        ))
    }
}

fn entry_str<'a>(entry: &'a Value, keys: &[&str]) -> &'a str {
    for key in keys {
        if let Some(s) = entry.get(*key).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return s.trim();
        }
    }
    ""
}

/// Python `_is_http_like_url`: http, https or lbry scheme.
fn is_http_like(url: &str) -> bool {
    url::Url::parse(url.trim())
        .map(|u| matches!(u.scheme(), "http" | "https" | "lbry"))
        .unwrap_or(false)
}

/// Python `_pick_ytdlp_search_entry_url`: the first
/// http-like of webpage_url, url, original_url. (Current
/// yt-dlp flat dumps carry the full page URL in `url`; a
/// bare extractor id is not a URL and is not repaired —
/// Python drops such entries.)
fn pick_entry_url(entry: &Value) -> String {
    for key in ["webpage_url", "url", "original_url"] {
        let val = entry.get(key).and_then(|v| v.as_str()).unwrap_or("").trim();
        if !val.is_empty() && is_http_like(val) {
            return val.to_string();
        }
    }
    String::new()
}

/// Python `_infer_ytdlp_search_result_kind`.
fn infer_kind(url: &str, entry: &Value, site_id: &str) -> &'static str {
    if crate::discovery::looks_like_feed_url(url) {
        return "feed";
    }
    let uploader_url = entry_str(entry, &["uploader_url"]).trim_end_matches('/');
    if !uploader_url.is_empty() && is_http_like(uploader_url) {
        let row_url = url.trim().trim_end_matches('/');
        if !row_url.is_empty()
            && row_url != uploader_url
            && row_url.starts_with(&format!("{uploader_url}/"))
        {
            return "media";
        }
    }
    if entry_str(entry, &["_type"]).eq_ignore_ascii_case("playlist") {
        return "playlist";
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return "media";
    };
    if parsed
        .query_pairs()
        .any(|(k, v)| k == "list" && !v.trim().is_empty())
    {
        return "playlist";
    }
    let path_low = parsed.path().to_lowercase();
    let path_trimmed = path_low.trim_end_matches('/');
    if [
        "/playlist",
        "/playlists",
        "/sets/",
        "/series",
        "/podcast",
        "/shows/",
        "/stack",
    ]
    .iter()
    .any(|tok| path_trimmed.contains(tok))
    {
        return "playlist";
    }
    let parts: Vec<String> = parsed
        .path()
        .split('/')
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.trim().to_lowercase())
        .collect();
    if let Some(first) = parts.first().map(String::as_str) {
        if ["channel", "channels", "profile", "profiles", "creator"].contains(&first)
            && parts.len() <= 2
        {
            return "user";
        }
        if ["user", "users"].contains(&first) && parts.len() <= 2 {
            return "user";
        }
    }
    if path_low.contains("/@") || path_low.starts_with("/@") {
        return "user";
    }
    if site_id.trim().eq_ignore_ascii_case("prxseries") {
        return "feed";
    }
    "media"
}

/// Python `_youtube_handle_from_url`: the `@handle` from a
/// YouTube profile URL's path.
fn youtube_handle_from_url(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return String::new();
    };
    let host = parsed.host_str().unwrap_or("");
    if !crate::discovery::host_matches(host, "youtube.com")
        && !crate::discovery::host_matches(host, "youtu.be")
    {
        return String::new();
    }
    let path = parsed.path();
    let Some(pos) = path.find("/@") else {
        return String::new();
    };
    let handle = path[pos + 2..].split(['/', '?', '#']).next().unwrap_or("");
    let handle = percent_decode(handle).trim().to_string();
    if handle.is_empty() {
        String::new()
    } else {
        format!("@{handle}")
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Python `_extract_ytdlp_search_entry_owner_label` with
/// `allow_oembed=False` (the search path): name fields in
/// order, then a strict handle from the id fields, then a
/// handle from the entry's channel-ish URLs or the row URL.
fn owner_label(entry: &Value, url: &str) -> String {
    let name = entry_str(
        entry,
        &[
            "channel",
            "uploader",
            "playlist_uploader",
            "artist",
            "creator",
            "author",
        ],
    )
    .to_string();
    let mut handle = String::new();
    for key in ["uploader_id", "channel_handle", "author_id", "creator_id"] {
        let raw = entry_str(entry, &[key]);
        if raw.is_empty() {
            continue;
        }
        if raw.starts_with('@') {
            handle = raw.to_string();
            break;
        }
        // Keep this strict to avoid converting arbitrary IDs
        // into handles (Python's rule).
        let strict = (2..=40).contains(&raw.chars().count())
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !raw.to_uppercase().starts_with("UC");
        if strict {
            handle = format!("@{raw}");
            break;
        }
    }
    if handle.is_empty() {
        for candidate in [
            entry_str(entry, &["channel_url"]),
            entry_str(entry, &["uploader_url"]),
            entry_str(entry, &["author_url"]),
            entry_str(entry, &["creator_url"]),
            url,
            entry_str(entry, &["webpage_url"]),
        ] {
            let found = youtube_handle_from_url(candidate);
            if !found.is_empty() {
                handle = found;
                break;
            }
        }
    }
    if !name.is_empty() && !handle.is_empty() {
        if name.trim_start_matches('@').to_lowercase()
            == handle.trim_start_matches('@').to_lowercase()
        {
            handle
        } else {
            format!("{name} / {handle}")
        }
    } else if !name.is_empty() {
        name
    } else {
        handle
    }
}

/// Python `_extract_ytdlp_search_entry_play_count`:
/// view_count then play_count; ints and float-shaped
/// strings; negatives keep looking.
fn play_count(entry: &Value) -> Option<i64> {
    for key in ["view_count", "play_count"] {
        let Some(raw) = entry.get(key) else {
            continue;
        };
        let val = raw.as_i64().or_else(|| {
            raw.as_f64().map(|f| f as i64).or_else(|| {
                raw.as_str()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|f| f as i64)
            })
        });
        if let Some(v) = val
            && v >= 0
        {
            return Some(v);
        }
    }
    None
}

/// Python `_URL_FALLBACK_SITE_NAMES`.
fn url_fallback_site_name(host: &str) -> Option<&'static str> {
    for (dom, label) in [
        ("youtube.com", "YouTube"),
        ("youtu.be", "YouTube"),
        ("facebook.com", "Facebook"),
        ("bilibili.com", "Bilibili"),
        ("rokfin.com", "Rokfin"),
        ("soundcloud.com", "SoundCloud"),
    ] {
        if crate::discovery::host_matches(host, dom) {
            return Some(label);
        }
    }
    None
}

/// Python `_display_site_label_from_result_url`: prefer the
/// label of the site the result URL actually belongs to.
fn display_site_label(url: &str, fallback_label: &str) -> String {
    let label = fallback_label.trim();
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return label.to_string();
    };
    let mut host = parsed.host_str().unwrap_or("").to_lowercase();
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }
    if host.starts_with("m.") && host.ends_with("facebook.com") {
        host = host[2..].to_string();
    }
    if let Some(found) = url_fallback_site_name(&host) {
        return found.to_string();
    }
    label.to_string()
}

/// Python `_friendly_title_fallback_from_url`: a readable
/// placeholder title for URL-only results.
fn friendly_title_fallback(url: &str, site_label: &str) -> String {
    let raw = url.trim();
    if raw.is_empty() {
        return String::new();
    }
    let Ok(parsed) = url::Url::parse(raw) else {
        return String::new();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return String::new();
    }
    let mut host = parsed.host_str().unwrap_or("").to_lowercase();
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }
    if host.starts_with("m.") && host.ends_with("facebook.com") {
        host = host[2..].to_string();
    }
    let path_parts: Vec<String> = parsed
        .path()
        .split('/')
        .filter(|p| !p.trim().is_empty())
        .map(|p| percent_decode(p).trim().to_string())
        .collect();
    let query_val = |name: &str| {
        parsed
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    };
    let mut site_name = site_label.trim().to_string();
    if site_name.is_empty() {
        site_name = url_fallback_site_name(&host)
            .map(|s| s.to_string())
            .unwrap_or_else(|| host.split(':').next().unwrap_or("Site").to_string());
        if site_name.is_empty() {
            site_name = "Site".to_string();
        }
    }
    let id_from_parts = |name: &str| -> String {
        if let Some(idx) = path_parts.iter().position(|p| p == name)
            && idx + 1 < path_parts.len()
        {
            return path_parts[idx + 1].trim().to_string();
        }
        String::new()
    };
    if crate::discovery::host_matches(&host, "youtube.com")
        || crate::discovery::host_matches(&host, "youtu.be")
    {
        if crate::discovery::host_matches(&host, "youtu.be") && !path_parts.is_empty() {
            return format!("YouTube video {}", path_parts[0]);
        }
        let vid = query_val("v");
        if !vid.is_empty() {
            return format!("YouTube video {vid}");
        }
        let pid = query_val("list");
        if !pid.is_empty() {
            return format!("YouTube playlist {pid}");
        }
    }
    if host.contains("facebook.com") {
        let reel = id_from_parts("reel");
        if !reel.is_empty() {
            return format!("Facebook reel {reel}");
        }
        let vid = query_val("v");
        if !vid.is_empty() {
            return format!("Facebook video {vid}");
        }
    }
    if host.contains("rokfin.com") {
        for kind in ["post", "stream", "stack"] {
            let item_id = id_from_parts(kind);
            if !item_id.is_empty() {
                return format!("Rokfin {kind} {item_id}");
            }
        }
    }
    if host.contains("bilibili.com")
        && let Some(idx) = path_parts.iter().position(|p| p == "video")
        && idx + 1 < path_parts.len()
    {
        return format!("Bilibili video {}", path_parts[idx + 1]);
    }
    let leaf = path_parts.last().cloned().unwrap_or_default();
    let leaf = leaf
        .split(['-', '_'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let leaf = leaf.split_whitespace().collect::<Vec<_>>().join(" ");
    if !leaf.is_empty() {
        if leaf.chars().count() > 80 {
            let cut: String = leaf.chars().take(77).collect();
            return format!("{site_name} {}...", cut.trim_end());
        }
        return format!("{site_name} {leaf}");
    }
    format!("{site_name} item")
}

/// Python `_build_ytdlp_search_result_detail`.
fn build_detail(entry: &Value, site_label: &str, kind: &str) -> String {
    let mut bits: Vec<String> = Vec::new();
    if !kind.is_empty() && kind != "media" {
        bits.push(title_case(kind));
    }
    let entry_url = entry_str(entry, &["webpage_url", "url"]).to_string();
    let owner = owner_label(entry, &entry_url);
    if !owner.is_empty() && !bits.contains(&owner) {
        bits.push(owner);
    }
    if let Some(duration) = entry.get("duration").and_then(|v| v.as_f64()) {
        let sec = duration as i64;
        if sec > 0 {
            let (h, rem) = (sec / 3600, sec % 3600);
            let (m, s) = (rem / 60, rem % 60);
            bits.push(if h > 0 {
                format!("{h}:{m:02}:{s:02}")
            } else {
                format!("{m}:{s:02}")
            });
        }
    }
    if bits.is_empty() {
        bits.push(site_label.to_string());
    }
    bits.join(" | ")
}

/// Python `_native_youtube_feed_from_search_entry`.
fn native_youtube_feed_from_entry(url: &str, entry: &Value, kind: &str, site_id: &str) -> String {
    let sid = site_id.trim().to_lowercase();
    if sid != "ytsearch" && sid != "youtube" {
        return String::new();
    }
    if kind == "playlist" {
        let mut playlist_id = entry_str(entry, &["playlist_id"]).to_string();
        if playlist_id.is_empty()
            && let Ok(parsed) = url::Url::parse(url)
        {
            playlist_id = parsed
                .query_pairs()
                .find(|(k, _)| k == "list")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
        }
        if !playlist_id.is_empty() {
            return format!("https://www.youtube.com/feeds/videos.xml?playlist_id={playlist_id}");
        }
    }
    if kind == "user" {
        let channel_id = entry_str(entry, &["channel_id"]);
        if !channel_id.is_empty() {
            return format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}");
        }
    }
    String::new()
}

/// Python `_choose_entry_subscribe_source_url`.
fn choose_entry_source_url(entry: &Value, site_id: &str) -> String {
    for key in [
        "channel_url",
        "uploader_url",
        "creator_url",
        "artist_url",
        "playlist_url",
    ] {
        let cand = entry_str(entry, &[key]);
        if !cand.is_empty() && is_http_like(cand) {
            return cand.to_string();
        }
    }
    let sid = site_id.trim().to_lowercase();
    if ["ytsearch", "youtube", "yvsearch", "yahoo"].contains(&sid.as_str()) {
        let channel_id = entry_str(entry, &["channel_id"]);
        if !channel_id.is_empty() {
            return format!("https://www.youtube.com/channel/{channel_id}");
        }
        let uploader_url = entry_str(entry, &["uploader_url"]);
        if !uploader_url.is_empty() && is_http_like(uploader_url) {
            return uploader_url.to_string();
        }
    }
    String::new()
}

/// Python `_derive_subscribe_targets_for_search_result`
/// (Audit 8 F05): the (native feed, source page) subscribe
/// targets for one result.
fn derive_targets(url: &str, entry: &Value, kind: &str, site_id: &str) -> (String, String) {
    let result_url = url.trim();
    match kind {
        "feed" => {
            if crate::discovery::looks_like_feed_url(result_url) {
                return (result_url.to_string(), result_url.to_string());
            }
            (
                String::new(),
                if is_http_like(result_url) {
                    result_url.to_string()
                } else {
                    String::new()
                },
            )
        }
        "user" | "playlist" => {
            let mut source = if is_http_like(result_url) {
                result_url.to_string()
            } else {
                String::new()
            };
            let mut native = native_youtube_feed_from_entry(result_url, entry, kind, site_id);
            if native.is_empty() && !source.is_empty() {
                native = crate::discovery::ytdlp_feed_url(&source).unwrap_or_default();
            }
            if source.is_empty() && !native.is_empty() {
                source = if is_http_like(result_url) {
                    result_url.to_string()
                } else {
                    String::new()
                };
            } else if !source.is_empty()
                && native.is_empty()
                && !crate::discovery::ytdlp_supported_offline(&source)
            {
                source = String::new();
            }
            (native, source)
        }
        "media" => {
            let mut source = choose_entry_source_url(entry, site_id);
            let mut effective_kind = kind;
            if source.is_empty() && is_http_like(result_url) {
                let media_kind = infer_kind(result_url, entry, site_id);
                if ["user", "playlist", "feed"].contains(&media_kind) {
                    source = result_url.to_string();
                    effective_kind = media_kind;
                }
            }
            let _ = effective_kind;
            let mut native = String::new();
            if !source.is_empty() {
                native = crate::discovery::ytdlp_feed_url(&source).unwrap_or_default();
                if native.is_empty() {
                    native = crate::discovery::social_feed_url(&source).unwrap_or_default();
                }
            }
            (native, source)
        }
        _ => (String::new(), String::new()),
    }
}

/// Python `_normalize_ytdlp_search_entries`, minus the
/// network quick-title prefetch (no offline equivalent;
/// junk titles still repair through the URL fallback).
/// Results dedupe by exact picked URL and stop at `limit`.
fn normalize_entries(json: &Value, site: &SearchSite, limit: usize) -> Vec<VideoResult> {
    let Some(entries) = json.get("entries").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let limit = limit.clamp(1, 500);
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<VideoResult> = Vec::new();
    for entry in entries {
        let url = pick_entry_url(entry);
        if url.is_empty() || seen.contains(&url) {
            continue;
        }
        seen.push(url.clone());
        let display_label = display_site_label(&url, &site.label);
        let kind = infer_kind(&url, entry, &site.id);
        let mut raw_title = entry_str(entry, &["title"]).to_string();
        if !raw_title.is_empty() {
            // Some flat entries carry a junk title equal to
            // the video id or the URL itself; treat those as
            // missing so the fallback can repair them.
            let entry_id = entry_str(entry, &["id"]);
            if raw_title == url || (!entry_id.is_empty() && raw_title == entry_id) {
                raw_title = String::new();
            }
        }
        let (title, title_is_fallback) = if !raw_title.is_empty() {
            (raw_title, false)
        } else {
            let fallback = friendly_title_fallback(&url, &display_label);
            let fallback = if fallback.is_empty() {
                url.clone()
            } else {
                fallback
            };
            let is_fallback = fallback != url;
            (fallback, is_fallback)
        };
        let (native_feed_url, source_subscribe_url) = derive_targets(&url, entry, kind, &site.id);
        let owner = owner_label(entry, &url);
        out.push(VideoResult {
            title,
            url,
            site: display_label.clone(),
            site_id: site.id.clone(),
            kind: kind.to_string(),
            play_count: play_count(entry),
            duration: entry.get("duration").and_then(|v| v.as_f64()),
            uploader: if owner.is_empty() { None } else { Some(owner) },
            detail: build_detail(entry, &display_label, kind),
            title_is_fallback,
            native_feed_url,
            source_subscribe_url,
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn search_mixcloud(term: &str, limit: usize) -> Result<Vec<VideoResult>, String> {
    // Python `search_mixcloud_media`: cloudcast search, the
    // user page as the subscribe source, detail
    // "Mixcloud • <author>".
    let limit = limit.clamp(1, 100);
    let url = format!(
        "https://api.mixcloud.com/search/?q={}&type=cloudcast&limit={}",
        percent_encode(term),
        limit
    );
    let mut resp = crate::http::agent()
        .get(&url)
        .call()
        .map_err(|e| format!("Mixcloud: {e}"))?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Mixcloud: {e}"))?;
    let json: Value = serde_json::from_str(&body).map_err(|e| format!("Mixcloud: {e}"))?;
    let Some(data) = json.get("data").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for e in data {
        let url = entry_str(e, &["url"]).to_string();
        if url.is_empty() || seen.contains(&url) {
            continue;
        }
        seen.push(url.clone());
        let user = e.get("user").cloned().unwrap_or(Value::Null);
        let author = entry_str(&user, &["name", "username"]).to_string();
        let title = {
            let name = entry_str(e, &["name"]);
            if name.is_empty() {
                url.clone()
            } else {
                name.to_string()
            }
        };
        let play = e
            .get("play_count")
            .or_else(|| e.get("listener_count"))
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .filter(|v| *v != 0);
        out.push(VideoResult {
            title,
            detail: if author.is_empty() {
                "Mixcloud".to_string()
            } else {
                format!("Mixcloud • {author}")
            },
            url,
            site: "Mixcloud".to_string(),
            site_id: "mixcloud".to_string(),
            kind: "media".to_string(),
            play_count: play,
            duration: None,
            uploader: if author.is_empty() {
                None
            } else {
                Some(author)
            },
            title_is_fallback: false,
            native_feed_url: String::new(),
            source_subscribe_url: entry_str(&user, &["url"]).to_string(),
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Search one site. `limit` is clamped to Python's 1..=500.
pub fn search_site(
    term: &str,
    site: &SearchSite,
    limit: usize,
) -> Result<Vec<VideoResult>, String> {
    let term = term.trim();
    if term.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 500);
    if site.provider.as_deref() == Some("mixcloud") {
        return search_mixcloud(term, limit);
    }
    let target = if let Some(tmpl) = &site.url_template {
        tmpl.replace("{query}", &percent_encode(term))
    } else if let Some(key) = &site.search_key {
        format!("{key}{limit}:{term}")
    } else {
        return Ok(Vec::new());
    };
    let args = vec![
        "--dump-single-json".to_string(),
        "--flat-playlist".to_string(),
        "--playlist-end".to_string(),
        limit.to_string(),
        target,
    ];
    let stdout = run_ytdlp(args, 30)?;
    let json: Value = serde_json::from_str(&stdout).map_err(|e| format!("yt-dlp output: {e}"))?;
    Ok(normalize_entries(&json, site, limit))
}

/// Stable identity for cross-site dedupe (Python
/// `canonical_search_result_key`, Audit 8 F18): YouTube
/// collapses to the video id WITH ITS CASE PRESERVED,
/// everything else to host + lowercased path + the query
/// exactly as written — only case-insensitive components
/// are normalized, so case-distinct ids and query tokens
/// stay distinct.
pub fn canonical_key(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_lowercase();
    };
    let host_full = parsed.host_str().unwrap_or("").to_lowercase();
    let host = host_full.strip_prefix("www.").unwrap_or(&host_full);
    if [
        "youtube.com",
        "m.youtube.com",
        "music.youtube.com",
        "youtu.be",
    ]
    .contains(&host)
    {
        let mut video_id = String::new();
        if host == "youtu.be" {
            video_id = parsed
                .path()
                .trim_matches('/')
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
        } else if let Some(pos) = parsed.path().find("/shorts/") {
            video_id = parsed.path()[pos + "/shorts/".len()..]
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
        } else if let Some(pos) = parsed.path().find("/embed/") {
            video_id = parsed.path()[pos + "/embed/".len()..]
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
        } else {
            for (k, v) in parsed.query_pairs() {
                if k == "v" {
                    video_id = v.into_owned();
                    break;
                }
            }
        }
        let video_id = video_id.trim();
        if !video_id.is_empty() {
            return format!("youtube:{video_id}");
        }
    }
    let path = parsed.path().trim_end_matches('/').to_lowercase();
    let key = format!("{host}{path}");
    match parsed.query() {
        Some(query) if !query.is_empty() => format!("{key}?{query}"),
        _ if key.is_empty() => url.to_lowercase(),
        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_keys_collapse_youtube_variants() {
        assert_eq!(
            canonical_key("https://www.youtube.com/watch?v=abc123&t=4"),
            "youtube:abc123"
        );
        assert_eq!(canonical_key("https://youtu.be/abc123"), "youtube:abc123");
        assert_eq!(
            canonical_key("https://www.youtube.com/shorts/abc123"),
            "youtube:abc123"
        );
        assert_eq!(canonical_key("https://vimeo.com/12345/"), "vimeo.com/12345");
    }

    #[test]
    fn canonical_keys_preserve_case_sensitive_parts() {
        // F18: YouTube ids and query tokens are case-sensitive;
        // only the host and path are case-insensitive.
        assert_ne!(
            canonical_key("https://www.youtube.com/watch?v=AbC123"),
            canonical_key("https://www.youtube.com/watch?v=abc123")
        );
        assert_eq!(
            canonical_key("https://EXAMPLE.com/Feed/?Token=AbC"),
            "example.com/feed?Token=AbC"
        );
        assert_ne!(
            canonical_key("https://example.com/feed?Token=AbC"),
            canonical_key("https://example.com/feed?token=abc")
        );
    }

    #[test]
    fn registry_derives_sites_from_extractor_names() {
        // F06: the site list follows the installed yt-dlp's
        // extractor names, not a hard-coded set. This fixture
        // is the search-name set yt-dlp 2026.08.19 prints.
        let sites = sites_from_extractor_names(FALLBACK_EXTRACTOR_NAMES, false);
        let labels: Vec<&str> = sites.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Bilibili",
                "Mixcloud",
                "Niconico",
                "PRX Series",
                "PRX Stories",
                "Rokfin",
                "SoundCloud",
                "YouTube"
            ]
        );
        let youtube = sites.iter().find(|s| s.label == "YouTube").unwrap();
        assert_eq!(youtube.id, "ytsearch");
        assert_eq!(youtube.search_key.as_deref(), Some("ytsearch"));
        // The Niconico date variant folds into the base site
        // and is retained as an alias.
        let nico = sites.iter().find(|s| s.label == "Niconico").unwrap();
        assert_eq!(nico.id, "nicosearch");
        assert_eq!(nico.search_key.as_deref(), Some("nicosearch"));
        assert_eq!(nico.search_key_aliases, ["nicosearchdate"]);
        // Google/Yahoo video search are redundant and dropped;
        // Vimeo/TikTok have no search extractor in this
        // yt-dlp, so they are not offered (Python agrees).
        assert!(
            sites
                .iter()
                .all(|s| s.id != "gvsearch" && s.id != "yvsearch")
        );
        assert!(sites.iter().all(|s| !s.adult));
    }

    #[test]
    fn registry_adult_sites_append_after_sort() {
        let sites = sites_from_extractor_names(FALLBACK_EXTRACTOR_NAMES, true);
        let last = sites.last().unwrap();
        assert_eq!(last.id, "pornhub");
        assert!(last.adult);
        assert!(
            last.url_template
                .as_deref()
                .unwrap_or("")
                .contains("pornhub.com")
        );
        assert!(
            sites_from_extractor_names(FALLBACK_EXTRACTOR_NAMES, false)
                .iter()
                .all(|s| !s.adult)
        );
    }

    #[test]
    fn registry_tracks_extra_extractors() {
        // A yt-dlp that gains a search extractor gains the
        // site; one that loses it loses the site.
        let mut names: Vec<&str> = FALLBACK_EXTRACTOR_NAMES.to_vec();
        names.retain(|n| *n != "rokfin:search");
        let sites = sites_from_extractor_names(&names, false);
        assert!(sites.iter().all(|s| s.label != "Rokfin"));
        let mut names: Vec<&str> = FALLBACK_EXTRACTOR_NAMES.to_vec();
        names.push("dailymotion:search");
        // dailymotion:search has no verified query key (its
        // _SEARCH_KEY is not CLI-visible), so it is enumerated
        // but not offered — the documented bridge limit.
        let sites = sites_from_extractor_names(&names, false);
        assert!(sites.iter().all(|s| s.label != "Dailymotion"));
    }

    #[test]
    fn adult_keyword_detection_matches_python() {
        assert!(is_adult_search_text(&[
            "xhamstersearch",
            "",
            "xhamster:search",
            ""
        ]));
        assert!(!is_adult_search_text(&[
            "ytsearch",
            "YoutubeSearch",
            "youtube:search",
            ""
        ]));
    }

    fn youtube_site() -> SearchSite {
        sites_from_extractor_names(FALLBACK_EXTRACTOR_NAMES, false)
            .into_iter()
            .find(|s| s.id == "ytsearch")
            .unwrap()
    }

    #[test]
    fn flat_entries_normalize() {
        let site = youtube_site();
        let json = serde_json::json!({"entries": [
            {"title": "A", "url": "https://www.youtube.com/watch?v=xyz9", "ie_key": "Youtube", "view_count": 42, "duration": 61.0, "uploader": "Chan"},
            {"title": "B", "url": "https://vimeo.com/7", "ie_key": "Vimeo", "view_count": null},
            {"title": "", "url": ""}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://www.youtube.com/watch?v=xyz9");
        assert_eq!(out[0].play_count, Some(42));
        assert_eq!(out[1].play_count, None);
    }

    #[test]
    fn normalize_derives_media_source_and_native_targets() {
        // F05: a media row keeps its channel as the source
        // target; with a channel_url the native feed resolves
        // without any network call.
        let site = youtube_site();
        let json = serde_json::json!({"entries": [
            {"title": "Video", "url": "https://www.youtube.com/watch?v=abc", "channel_url": "https://www.youtube.com/channel/UCxyz", "channel": "Chan", "view_count": 7}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.kind, "media");
        assert_eq!(
            r.source_subscribe_url,
            "https://www.youtube.com/channel/UCxyz"
        );
        assert_eq!(
            r.native_feed_url,
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCxyz"
        );
        assert_eq!(r.best_subscribe_url(), Some(r.native_feed_url.as_str()));
        assert_eq!(r.detail, "Chan");
    }

    #[test]
    fn normalize_derives_user_and_playlist_targets() {
        let site = youtube_site();
        let json = serde_json::json!({"entries": [
            {"title": "Chan", "url": "https://www.youtube.com/@somehandle", "channel_id": "UC123"},
            {"title": "List", "url": "https://www.youtube.com/playlist?list=PL9", "_type": "playlist"}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out[0].kind, "user");
        assert_eq!(
            out[0].native_feed_url,
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC123"
        );
        assert_eq!(
            out[0].source_subscribe_url,
            "https://www.youtube.com/@somehandle"
        );
        assert_eq!(out[1].kind, "playlist");
        assert_eq!(
            out[1].native_feed_url,
            "https://www.youtube.com/feeds/videos.xml?playlist_id=PL9"
        );
    }

    #[test]
    fn normalize_repairs_junk_titles() {
        // F19: a title equal to the id or the URL is junk and
        // is repaired from the URL; the row is marked as a
        // fallback title so a real duplicate can outrank it.
        let site = youtube_site();
        let json = serde_json::json!({"entries": [
            {"title": "dQw4w9WgXcQ", "id": "dQw4w9WgXcQ", "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ"},
            {"title": "https://www.youtube.com/watch?v=zz9", "url": "https://www.youtube.com/watch?v=zz9"}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out[0].title, "YouTube video dQw4w9WgXcQ");
        assert!(out[0].title_is_fallback);
        assert_eq!(out[1].title, "YouTube video zz9");
        assert!(out[1].title_is_fallback);
    }

    #[test]
    fn normalize_owner_label_and_play_count_rules() {
        // F19: owner combines name and handle; UC-prefixed ids
        // never become handles; float-shaped counts parse.
        let site = youtube_site();
        let json = serde_json::json!({"entries": [
            {"title": "V", "url": "https://www.youtube.com/watch?v=q1", "channel": "Free Code", "uploader_id": "@freecode", "view_count": "12345.0"},
            {"title": "W", "url": "https://www.youtube.com/watch?v=q2", "uploader_id": "UC8butISFwT-Wl7EV0hUK0BQ", "play_count": -3}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out[0].uploader.as_deref(), Some("Free Code / @freecode"));
        assert_eq!(out[0].play_count, Some(12345));
        assert_eq!(out[1].uploader, None);
        assert_eq!(out[1].play_count, None);
    }

    #[test]
    fn merge_prefers_better_duplicate() {
        // F19: a later duplicate with a real title and a
        // subscribe target replaces the stored fallback row
        // in place; a worse one does not.
        let site = youtube_site();
        let weak = normalize_entries(
            &serde_json::json!({"entries": [
                {"title": "", "url": "https://www.youtube.com/watch?v=same1"}
            ]}),
            &site,
            10,
        )
        .remove(0);
        let mut results = vec![weak];
        let strong = normalize_entries(
            &serde_json::json!({"entries": [
                {"title": "Real Title", "url": "https://youtu.be/same1", "channel_url": "https://www.youtube.com/channel/UCz", "view_count": 50}
            ]}),
            &site,
            10,
        )
        .remove(0);
        merge_result(&mut results, strong);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Real Title");
        assert_eq!(results[0].play_count, Some(50));
        let worse = normalize_entries(
            &serde_json::json!({"entries": [
                {"title": "", "url": "https://www.youtube.com/watch?v=same1"}
            ]}),
            &site,
            10,
        )
        .remove(0);
        merge_result(&mut results, worse);
        assert_eq!(results[0].title, "Real Title");
    }
}
