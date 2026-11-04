//! Cross-site video search (Python `discovery.search_ytdlp_site`
//! + the site set behind the YtdlpGlobalSearchDialog).
//!
//! Python derives its site list from the installed yt-dlp's
//! extractor registry at runtime; the Rust port ships the
//! mainstream set the dialog prioritises (YouTube, SoundCloud,
//! Bilibili, Vimeo, TikTok, Niconico — Google/Yahoo video search
//! are dropped exactly as Python drops them for duplicating
//! YouTube), plus Mixcloud's public API and, only when the user
//! enables adult search, the Pornhub search-page template.
//! Each site search is one yt-dlp flat-playlist dump with a
//! hard timeout, so a hung extractor cannot stall the dialog.

/// One searchable site.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchSite {
    pub id: &'static str,
    pub label: &'static str,
    /// yt-dlp query-search key (e.g. "ytsearch"), when the site
    /// is searched by query.
    pub search_key: Option<&'static str>,
    /// Search-results page template with a `{query}` slot, for
    /// sites yt-dlp cannot query-search.
    pub url_template: Option<&'static str>,
    /// Searched through a dedicated provider API instead.
    pub provider: Option<&'static str>,
    pub adult: bool,
}

/// The sites offered in Video Search. Adult sites appear only
/// with `include_adult` (config `enable_adult_search`).
pub fn searchable_sites(include_adult: bool) -> Vec<SearchSite> {
    let mut sites = vec![
        SearchSite {
            id: "youtube",
            label: "YouTube",
            search_key: Some("ytsearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "soundcloud",
            label: "SoundCloud",
            search_key: Some("scsearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "bilibili",
            label: "Bilibili",
            search_key: Some("bilisearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "vimeo",
            label: "Vimeo",
            search_key: Some("vimeosearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "tiktok",
            label: "TikTok",
            search_key: Some("tiktoksearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "niconico",
            label: "Niconico",
            search_key: Some("nicosearch"),
            url_template: None,
            provider: None,
            adult: false,
        },
        SearchSite {
            id: "mixcloud",
            label: "Mixcloud",
            search_key: None,
            url_template: None,
            provider: Some("mixcloud"),
            adult: false,
        },
    ];
    if include_adult {
        sites.push(SearchSite {
            id: "pornhub",
            label: "Pornhub",
            search_key: None,
            url_template: Some("https://www.pornhub.com/video/search?search={query}"),
            provider: None,
            adult: true,
        });
    }
    sites
}

/// One normalized search result.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoResult {
    pub title: String,
    pub url: String,
    pub site: String,
    pub play_count: Option<i64>,
    pub duration: Option<f64>,
    pub uploader: Option<String>,
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

/// Repair a flat-playlist entry URL: YouTube flat entries carry
/// the bare video id, other sites carry real URLs.
fn entry_url(entry: &serde_json::Value) -> String {
    let raw = entry
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw;
    }
    let ie = entry.get("ie_key").and_then(|v| v.as_str()).unwrap_or("");
    if ie.to_lowercase().contains("youtube") && !raw.is_empty() {
        return format!("https://www.youtube.com/watch?v={raw}");
    }
    entry
        .get("webpage_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&raw)
        .to_string()
}

fn normalize_entries(
    json: &serde_json::Value,
    site: &SearchSite,
    limit: usize,
) -> Vec<VideoResult> {
    let Some(entries) = json.get("entries").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let url = entry_url(e);
            if url.is_empty() {
                return None;
            }
            let title = e
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            Some(VideoResult {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                site: site.label.to_string(),
                play_count: e.get("view_count").and_then(|v| v.as_i64()),
                duration: e.get("duration").and_then(|v| v.as_f64()),
                uploader: e
                    .get("uploader")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            })
        })
        .take(limit)
        .collect()
}

fn search_mixcloud(term: &str, limit: usize) -> Result<Vec<VideoResult>, String> {
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
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Mixcloud: {e}"))?;
    let Some(data) = json.get("data").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(data
        .iter()
        .filter_map(|e| {
            let url = e.get("url")?.as_str()?.to_string();
            Some(VideoResult {
                title: e.get("name")?.as_str()?.to_string(),
                url,
                site: "Mixcloud".to_string(),
                play_count: e.get("play_count").and_then(|v| v.as_i64()),
                duration: e.get("audio_length").and_then(|v| v.as_f64()),
                uploader: e
                    .get("user")
                    .and_then(|u| u.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            })
        })
        .collect())
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
    if site.provider == Some("mixcloud") {
        return search_mixcloud(term, limit);
    }
    let target = if let Some(tmpl) = site.url_template {
        tmpl.replace("{query}", &percent_encode(term))
    } else if let Some(key) = site.search_key {
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
    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("yt-dlp output: {e}"))?;
    Ok(normalize_entries(&json, site, limit))
}

/// Stable identity for cross-site dedupe (Python
/// `canonical_search_result_key`): YouTube collapses to the
/// video id, everything else to host + path (+ query).
pub fn canonical_key(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    let lower = url.to_lowercase();
    let no_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let (host_path, query) = match no_scheme.split_once('?') {
        Some((hp, q)) => (hp, q),
        None => (no_scheme, ""),
    };
    let (host, path) = match host_path.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (host_path, String::new()),
    };
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host == "youtube.com" || host == "m.youtube.com" || host == "music.youtube.com" {
        let mut vid = "";
        if let Some(rest) = path.strip_prefix("/shorts/") {
            vid = rest.split('/').next().unwrap_or("");
        } else if let Some(rest) = path.strip_prefix("/embed/") {
            vid = rest.split('/').next().unwrap_or("");
        } else {
            for pair in query.split('&') {
                if let Some(v) = pair.strip_prefix("v=") {
                    vid = v;
                    break;
                }
            }
        }
        if !vid.is_empty() {
            return format!("youtube:{vid}");
        }
    }
    if host == "youtu.be" {
        let vid = path.trim_start_matches('/').split('/').next().unwrap_or("");
        if !vid.is_empty() {
            return format!("youtube:{vid}");
        }
    }
    let path = path.trim_end_matches('/');
    if query.is_empty() {
        format!("{host}{path}")
    } else {
        format!("{host}{path}?{query}")
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
    fn adult_sites_gated() {
        assert!(searchable_sites(false).iter().all(|s| !s.adult));
        assert!(searchable_sites(true).iter().any(|s| s.adult));
    }

    #[test]
    fn flat_entries_normalize() {
        let site = searchable_sites(false).remove(0);
        let json = serde_json::json!({"entries": [
            {"title": "A", "url": "xyz9", "ie_key": "Youtube", "view_count": 42, "duration": 61.0, "uploader": "Chan"},
            {"title": "B", "url": "https://vimeo.com/7", "ie_key": "Vimeo", "view_count": null},
            {"title": "", "url": ""}
        ]});
        let out = normalize_entries(&json, &site, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://www.youtube.com/watch?v=xyz9");
        assert_eq!(out[0].play_count, Some(42));
        assert_eq!(out[1].play_count, None);
    }
}
