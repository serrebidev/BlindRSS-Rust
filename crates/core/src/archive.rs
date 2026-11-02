//! Podcast archive rescan (scoped port of `core/podcast_archive.py`).
//!
//! Python's archive follows feed lineage through Wayback snapshots
//! to recover episodes the live feed document no longer carries.
//! Current documents take precedence over older snapshots. Feed migrations
//! are followed with bounded lineage, snapshot count, and document sizes.

use crate::db::{Db, Feed};
use ureq::ResponseExt;

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
            if let Some(url) = public_url(&value) {
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

fn scan(
    seed: &str,
    limit: usize,
    mut fetch: impl FnMut(&str) -> Result<(Vec<u8>, String), String>,
) -> Result<Vec<crate::feeds::ParsedFeed>, String> {
    use std::collections::{HashSet, VecDeque};
    let seed =
        public_url(seed).ok_or("Podcast archive requires a public HTTP or HTTPS feed URL")?;
    let mut pending = VecDeque::from([seed.clone()]);
    let mut seen_urls = HashSet::from([seed]);
    let mut seen_entries = HashSet::new();
    let mut feeds = Vec::new();
    let mut remaining = limit.clamp(1, 5000);
    let mut first_error = None;
    let mut processed = 0;
    while let Some(feed_url) = pending.pop_front() {
        if processed >= 12 {
            break;
        }
        processed += 1;
        let mut documents = Vec::new();
        match fetch(&feed_url) {
            Ok(document) => documents.push(document),
            Err(error) => {
                first_error.get_or_insert(error);
            }
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
        if remaining > 0 {
            match fetch(index.as_str()) {
                Ok((bytes, _)) => {
                    for (timestamp, original) in snapshot_rows(&bytes, remaining) {
                        remaining -= 1;
                        let snapshot =
                            format!("https://web.archive.org/web/{timestamp}id_/{original}");
                        match fetch(&snapshot) {
                            Ok((bytes, _)) => documents.push((bytes, original)),
                            Err(error) => {
                                first_error.get_or_insert(error);
                            }
                        }
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        for (document, final_url) in documents {
            if document.len() > 10 * 1024 * 1024 {
                continue;
            }
            for candidate in
                std::iter::once(final_url.clone()).chain(lineage(&document, &final_url))
            {
                if seen_urls.len() < 12
                    && public_url(&candidate).is_some()
                    && seen_urls.insert(candidate.clone())
                {
                    pending.push_back(candidate);
                }
            }
            if let Ok(mut feed) = crate::feeds::parse_feed(&document) {
                feed.entries.retain(|entry| {
                    let keys = [
                        Some(format!("id:{}", entry.id)),
                        entry.media_url.as_ref().map(|u| format!("media:{u}")),
                    ];
                    let fresh = !keys.iter().flatten().any(|key| seen_entries.contains(key));
                    seen_entries.extend(keys.into_iter().flatten());
                    fresh
                });
                feeds.push(feed);
            }
        }
    }
    if feeds.is_empty() {
        Err(first_error
            .unwrap_or_else(|| "No readable live or archived podcast feed was found".into()))
    } else {
        Ok(feeds)
    }
}

/// Rescan the live document, archived snapshots and migrated feed URLs.
/// Returns the number of newly stored articles.
pub fn rescan_archive(db: &Db, feed: &Feed) -> Result<usize, String> {
    let documents = scan(&feed.url, 5000, |url| {
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
    })?;
    let mut total = 0;
    for document in documents {
        total += crate::refresh::store_parsed(db, feed, &document)?;
    }
    Ok(total)
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
