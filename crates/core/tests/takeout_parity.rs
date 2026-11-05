//! Audit 8 Task 27 (F11): YouTube Takeout parsing keeps
//! (source, URL) provenance — the same channel feed may appear
//! once per source, per-source counts survive, and `selected`
//! counts a source's feeds before URL deduplication (Python
//! `core/youtube_takeout.py`).

use blindrss_core::takeout::parse_takeout_zip;
use std::io::Write;

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in members {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }
    buf
}

const SUBS: &[u8] = b"Channel ID,Channel URL,Channel title\n\
UCaaaaaaaaaaaaaaaaaaaaaa,https://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa,Alpha\n\
UCbbbbbbbbbbbbbbbbbbbbbb,https://www.youtube.com/channel/UCbbbbbbbbbbbbbbbbbbbbbb,Beta\n";

const OWNER: &[u8] = b"Channel ID,Channel title (Original)\n\
UCaaaaaaaaaaaaaaaaaaaaaa,Alpha (mine)\n";

const PLAYLISTS: &[u8] = b"Playlist ID,Playlist title (original)\n\
PLxxxxxxxxxxxxxxxx,My List\n";

const HISTORY: &[u8] = b"<html><body>\
<a href=\"https://www.youtube.com/channel/UCbbbbbbbbbbbbbbbbbbbbbb\">Beta</a>\
<a href=\"https://www.youtube.com/channel/UCcccccccccccccccccccccc\">Gamma</a>\
</body></html>";

fn full_takeout() -> Vec<u8> {
    zip_bytes(&[
        (
            "Takeout/YouTube and YouTube Music/subscriptions/subscriptions.csv",
            SUBS,
        ),
        (
            "Takeout/YouTube and YouTube Music/channels/channel.csv",
            OWNER,
        ),
        (
            "Takeout/YouTube and YouTube Music/playlists/playlists.csv",
            PLAYLISTS,
        ),
        (
            "Takeout/YouTube and YouTube Music/history/watch-history.html",
            HISTORY,
        ),
        (
            "Takeout/Other/ignored.csv",
            b"Channel ID\nUCzzzzzzzzzzzzzzzzzzzzzz\n",
        ),
    ])
}

#[test]
fn same_url_kept_once_per_source_with_counts() {
    let parsed = parse_takeout_zip(&full_takeout()).unwrap();
    // Alpha's feed URL appears under subscriptions AND owner;
    // Beta's under subscriptions AND history. (source, URL)
    // dedupe keeps all of them (Python's rule); the old global
    // URL dedupe dropped the later sightings.
    assert_eq!(parsed.subscriptions, 2);
    assert_eq!(parsed.history_channels, 2);
    assert_eq!(parsed.owner_channels, 1);
    assert_eq!(parsed.playlists, 1);
    assert_eq!(parsed.feeds.len(), 6);
    let alpha: Vec<_> = parsed
        .feeds
        .iter()
        .filter(|f| f.title.starts_with("Alpha"))
        .collect();
    assert_eq!(alpha.len(), 2);
    let sources: Vec<&str> = alpha.iter().map(|f| f.source.as_str()).collect();
    assert!(sources.contains(&"subscriptions") && sources.contains(&"owner"));
    // The non-YouTube section member is ignored.
    assert!(!parsed.feeds.iter().any(|f| f.url.contains("UCzzzz")));
}

#[test]
fn selected_counts_before_url_dedupe_and_dedupes_by_url() {
    let parsed = parse_takeout_zip(&full_takeout()).unwrap();
    let all = parsed.selected(&["subscriptions", "history", "owner", "playlists"]);
    // Counts are the per-source tallies from before URL dedupe.
    assert_eq!(all.subscriptions, 2);
    assert_eq!(all.history_channels, 2);
    assert_eq!(all.owner_channels, 1);
    assert_eq!(all.playlists, 1);
    // Feeds are URL-unique, first occurrence wins: Alpha (subs),
    // Beta (subs), Gamma (history), the playlist.
    let urls: Vec<&str> = all.feeds.iter().map(|f| f.url.as_str()).collect();
    assert_eq!(urls.len(), 4);
    assert_eq!(
        std::collections::HashSet::<_>::from_iter(urls.iter()).len(),
        4
    );
    let alpha = all
        .feeds
        .iter()
        .find(|f| f.url.contains("UCaaaaaaaa"))
        .unwrap();
    assert_eq!(alpha.source, "subscriptions");

    let subs_only = parsed.selected(&["subscriptions"]);
    assert_eq!(subs_only.feeds.len(), 2);
    assert_eq!(subs_only.subscriptions, 2);
    assert_eq!(subs_only.history_channels, 0);
    assert_eq!(subs_only.playlists, 0);
}

#[test]
fn takeout_opml_is_flat_url_unique_with_titles() {
    let parsed = parse_takeout_zip(&full_takeout()).unwrap();
    let selected = parsed.selected(&["subscriptions", "history"]);
    let opml = selected.to_opml();
    assert!(opml.contains("<title>YouTube Takeout</title>"), "{opml}");
    // Alpha, Beta, Gamma — Beta appears once despite two sources.
    assert_eq!(opml.matches("<outline").count(), 3);
    assert!(opml.contains(
        "xmlUrl=\"https://www.youtube.com/feeds/videos.xml?channel_id=UCaaaaaaaaaaaaaaaaaaaaaa\""
    ));
    assert!(opml.contains("text=\"Gamma\""));
    assert!(opml.contains("type=\"rss\""));
}

#[test]
fn member_paths_must_match_python_exactly() {
    // A subscriptions.csv NOT at subscriptions/subscriptions.csv
    // under the YouTube section is not a subscriptions source in
    // Python (the old loose contains-match accepted it).
    let loose = zip_bytes(&[("Takeout/YouTube and YouTube Music/subscriptions.csv", SUBS)]);
    let err = parse_takeout_zip(&loose).unwrap_err();
    assert!(err.contains("No subscribable"), "{err}");

    // Playlists come only from playlists/playlists.csv, not from
    // arbitrary files under playlists/.
    let loose_pl = zip_bytes(&[(
        "Takeout/YouTube and YouTube Music/playlists/Some List.csv",
        PLAYLISTS,
    )]);
    let err = parse_takeout_zip(&loose_pl).unwrap_err();
    assert!(err.contains("No subscribable"), "{err}");
}

#[test]
fn history_channel_ids_have_no_length_or_charset_gate() {
    // Python's _channel_id_from_url takes path segment 2 as-is.
    let history: &[u8] = b"<html><body>\
<a href=\"https://www.youtube.com/channel/UCshort\">Shorty</a>\
</body></html>";
    let parsed = parse_takeout_zip(&zip_bytes(&[(
        "Takeout/YouTube and YouTube Music/history/watch-history.html",
        history,
    )]))
    .unwrap();
    assert_eq!(parsed.history_channels, 1);
    assert_eq!(
        parsed.feeds[0].url,
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCshort"
    );
    assert_eq!(parsed.feeds[0].title, "Shorty");
}

#[test]
fn takeout_errors_match_python_messages() {
    let err = parse_takeout_zip(b"not a zip").unwrap_err();
    assert_eq!(err, "The selected file is not a valid ZIP archive.");
    let no_yt = zip_bytes(&[("Takeout/Photos/x.csv", SUBS)]);
    let err = parse_takeout_zip(&no_yt).unwrap_err();
    assert_eq!(
        err,
        "No YouTube and YouTube Music export was found in this archive."
    );
}

#[test]
fn empty_channel_id_rows_are_skipped_and_title_falls_back_to_url() {
    let subs: &[u8] = b"Channel ID,Channel URL,Channel title\n\
,https://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa,No ID\n\
UCdddddddddddddddddddddd,https://www.youtube.com/channel/UCdddddddddddddddddddddd,\n";
    let parsed = parse_takeout_zip(&zip_bytes(&[(
        "Takeout/YouTube and YouTube Music/subscriptions/subscriptions.csv",
        subs,
    )]))
    .unwrap();
    // Python reads only the Channel ID column (no URL fallback),
    // so the ID-less row is dropped; the untitled row's title
    // falls back to the feed URL (Python's add() rule).
    assert_eq!(parsed.subscriptions, 1);
    assert_eq!(
        parsed.feeds[0].title,
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCdddddddddddddddddddddd"
    );
}
