//! Batch 9: cast discovery parsers (UPnP description, mDNS).

use blindrss_core::cast::{mdns_ptr_query, parse_mdns_response, parse_upnp_description};

#[test]
fn upnp_description_parse() {
    let xml = r#"<root><device><friendlyName>Living Room TV</friendlyName><modelName>TV</modelName></device><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/upnp/control/avt</controlURL></service></serviceList></root>"#;
    let (name, control, _model) =
        parse_upnp_description(xml, "http://192.168.1.5:9197/dmr.xml").unwrap();
    assert_eq!(name, "Living Room TV");
    assert_eq!(control, "http://192.168.1.5:9197/upnp/control/avt");
    // No AVTransport -> None.
    let xml2 = "<root><device><friendlyName>X</friendlyName></device></root>";
    assert!(parse_upnp_description(xml2, "http://h/d.xml").is_none());
}

#[test]
fn mdns_round_trip() {
    // Build a response: PTR _kodi-jsonrpc._tcp.local -> Kodi-1,
    // SRV Kodi-1 -> kodi.local:8080, A kodi.local -> 192.168.1.9.
    fn name_into(out: &mut Vec<u8>, name: &str) {
        for part in name.split('.') {
            out.push(part.len() as u8);
            out.extend_from_slice(part.as_bytes());
        }
        out.push(0);
    }
    let mut pkt = vec![0u8; 12];
    pkt[6] = 0;
    pkt[7] = 3; // ancount = 3
    // PTR record
    name_into(&mut pkt, "_kodi-jsonrpc._tcp.local");
    pkt.extend_from_slice(&12u16.to_be_bytes());
    pkt.extend_from_slice(&1u16.to_be_bytes());
    pkt.extend_from_slice(&120u32.to_be_bytes());
    let target_len_pos = pkt.len();
    pkt.extend_from_slice(&0u16.to_be_bytes());
    let target_start = pkt.len();
    name_into(&mut pkt, "Kodi-1._kodi-jsonrpc._tcp.local");
    let rdlen = (pkt.len() - target_start) as u16;
    pkt[target_len_pos] = (rdlen >> 8) as u8;
    pkt[target_len_pos + 1] = (rdlen & 0xff) as u8;
    // SRV record
    name_into(&mut pkt, "Kodi-1._kodi-jsonrpc._tcp.local");
    pkt.extend_from_slice(&33u16.to_be_bytes());
    pkt.extend_from_slice(&1u16.to_be_bytes());
    pkt.extend_from_slice(&120u32.to_be_bytes());
    let srv_len_pos = pkt.len();
    pkt.extend_from_slice(&0u16.to_be_bytes());
    let srv_start = pkt.len();
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&8080u16.to_be_bytes());
    name_into(&mut pkt, "kodi.local");
    let rdlen = (pkt.len() - srv_start) as u16;
    pkt[srv_len_pos] = (rdlen >> 8) as u8;
    pkt[srv_len_pos + 1] = (rdlen & 0xff) as u8;
    // A record
    name_into(&mut pkt, "kodi.local");
    pkt.extend_from_slice(&1u16.to_be_bytes());
    pkt.extend_from_slice(&1u16.to_be_bytes());
    pkt.extend_from_slice(&120u32.to_be_bytes());
    pkt.extend_from_slice(&4u16.to_be_bytes());
    pkt.extend_from_slice(&[192, 168, 1, 9]);

    let found = parse_mdns_response(&pkt);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "192.168.1.9");
    assert_eq!(found[0].2, 8080);
    // The query builder produces a parseable header.
    let q = mdns_ptr_query("_kodi-jsonrpc._tcp.local");
    assert!(q.len() > 12);
}

// Article change history (Python article_versions): record /
// dedupe / list, seeded by insert, extended on changed
// re-insert, plus the smart-folder opened/updated id sets.

fn scratch9(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn art9(id: &str, feed: &str, content: &str) -> blindrss_core::db::Article {
    blindrss_core::db::Article {
        id: id.into(),
        feed_id: feed.into(),
        title: "Title".into(),
        url: format!("https://example.com/{id}"),
        content: Some(content.into()),
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    }
}

#[test]
fn article_version_record_dedupe_and_list() {
    let path = scratch9("blindrss_batch9_versions.db");
    let db = blindrss_core::db::Db::open_or_create(&path).unwrap();
    assert!(db.record_article_version("a1", "T", Some("one")).unwrap());
    // Same (title, content) is a no-op.
    assert!(!db.record_article_version("a1", "T", Some("one")).unwrap());
    assert!(db.record_article_version("a1", "T", Some("two")).unwrap());
    // A title change alone is a new version.
    assert!(db.record_article_version("a1", "T2", Some("two")).unwrap());
    assert_eq!(db.count_article_versions("a1").unwrap(), 3);
    let versions = db.article_versions("a1").unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].title, "T2");
    assert_eq!(versions[2].content, "one");
    assert!(db.updated_article_ids().unwrap().contains("a1"));
    assert_eq!(db.count_article_versions("nope").unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn insert_seeds_and_refresh_extends_versions() {
    let path = scratch9("blindrss_batch9_versions2.db");
    let db = blindrss_core::db::Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    assert!(
        db.insert_article_if_new(&art9("x1", &feed.id, "v1 body"))
            .unwrap()
    );
    assert_eq!(db.count_article_versions("x1").unwrap(), 1);
    assert!(!db.updated_article_ids().unwrap().contains("x1"));
    // Same article arriving again unchanged: no insert, no version.
    assert!(
        !db.insert_article_if_new(&art9("x1", &feed.id, "v1 body"))
            .unwrap()
    );
    assert_eq!(db.count_article_versions("x1").unwrap(), 1);
    // Changed body on a later fetch: version appended.
    assert!(
        !db.insert_article_if_new(&art9("x1", &feed.id, "v2 body"))
            .unwrap()
    );
    assert_eq!(db.count_article_versions("x1").unwrap(), 2);
    assert!(db.updated_article_ids().unwrap().contains("x1"));
    // Opened set follows mark_opened.
    assert!(!db.opened_article_ids().unwrap().contains("x1"));
    db.mark_opened("x1").unwrap();
    assert!(db.opened_article_ids().unwrap().contains("x1"));
    let _ = std::fs::remove_file(&path);
}
