//! Podcast archive rescan (scoped port of `core/podcast_archive.py`).
//!
//! Python's archive follows feed lineage through Wayback snapshots
//! to recover episodes the live feed document no longer carries.
//! This port covers the main recovery path: parse the live feed
//! document fully, plus the newest good Wayback snapshot of the
//! feed URL, and store the union (article ids dedupe naturally).

use crate::db::{Db, Feed};

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

fn wayback_snapshot_url(feed_url: &str) -> Option<String> {
    let encoded: String = feed_url
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' | ':' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect();
    let cdx = format!(
        "https://web.archive.org/cdx/search/cdx?url={encoded}&output=json&limit=20&filter=statuscode:200"
    );
    let mut resp = ureq::get(&cdx).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let stamps = parse_cdx_timestamps(&text);
    let latest = stamps.last()?;
    Some(format!(
        "https://web.archive.org/web/{latest}id_/{feed_url}"
    ))
}

/// Rescan a feed's archive: live document + newest Wayback
/// snapshot, storing every episode not already present.
/// Returns the number of newly stored articles.
pub fn rescan_archive(db: &Db, feed: &Feed) -> Result<usize, String> {
    let mut total = 0;
    let live = crate::feeds::fetch_and_parse(&feed.url)?;
    total += crate::refresh::store_parsed(db, feed, &live)?;
    if let Some(snapshot) = wayback_snapshot_url(&feed.url)
        && let Ok(old) = crate::feeds::fetch_and_parse(&snapshot)
    {
        total += crate::refresh::store_parsed(db, feed, &old)?;
    }
    Ok(total)
}
