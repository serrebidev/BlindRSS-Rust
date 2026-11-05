//! Cast discovery parsing tests.

#[test]
fn ssdp_chromecast_packet() {
    let packet = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLOCATION: http://192.168.1.42:8008/ssdp/device-desc.xml\r\nSERVER: Linux/3.8.13 CastSDK\r\nST: urn:dial-multiscreen-org:service:dial:1\r\nX-Friendly-Name: Living Room TV\r\nX-Model-Name: Chromecast Ultra\r\n\r\n";
    let dev = blindrss_core::cast::parse_ssdp_packet(packet).unwrap();
    assert_eq!(dev.host, "192.168.1.42");
    assert_eq!(dev.port, 8009);
    assert_eq!(dev.name, "Living Room TV");
    assert_eq!(dev.model, "Chromecast Ultra");
    assert_eq!(dev.kind, "chromecast");
}

#[test]
fn ssdp_non_cast_ignored() {
    let packet = "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:5000/rootDesc.xml\r\nST: upnp:rootdevice\r\n\r\n";
    assert!(blindrss_core::cast::parse_ssdp_packet(packet).is_none());
}

// ---------------------------------------------------------------------------
// Task 28 (C02/C04/C05): record-level mDNS fixtures.
// ---------------------------------------------------------------------------

fn dns_name(out: &mut Vec<u8>, name: &str) {
    for part in name.split('.') {
        if part.is_empty() {
            continue;
        }
        out.push(part.len() as u8);
        out.extend_from_slice(part.as_bytes());
    }
    out.push(0);
}

fn mdns_packet(records: &[(&str, u16, Vec<u8>)]) -> Vec<u8> {
    let mut pkt = vec![0u8; 12];
    pkt[2] = 0x84; // response, authoritative
    pkt[6] = (records.len() >> 8) as u8;
    pkt[7] = records.len() as u8;
    for (name, rtype, rdata) in records {
        dns_name(&mut pkt, name);
        pkt.extend_from_slice(&rtype.to_be_bytes());
        pkt.extend_from_slice(&1u16.to_be_bytes()); // IN
        pkt.extend_from_slice(&120u32.to_be_bytes()); // TTL
        pkt.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        pkt.extend_from_slice(rdata);
    }
    pkt
}

fn ptr_rdata(target: &str) -> Vec<u8> {
    let mut out = Vec::new();
    dns_name(&mut out, target);
    out
}

fn srv_rdata(port: u16, target: &str) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    out[4] = (port >> 8) as u8;
    out[5] = port as u8;
    dns_name(&mut out, target);
    out
}

fn txt_rdata(entries: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.push(e.len() as u8);
        out.extend_from_slice(e.as_bytes());
    }
    out
}

fn googlecast_packet(instance: &str, fn_name: &str, ip: [u8; 4]) -> Vec<u8> {
    let target = format!(
        "chromecast-{}.local",
        instance.split('.').next().unwrap_or("x")
    );
    mdns_packet(&[
        ("_googlecast._tcp.local", 12, ptr_rdata(instance)),
        (instance, 33, srv_rdata(8009, &target)),
        (
            instance,
            16,
            txt_rdata(&["id=abc123", "md=Chromecast Ultra", &format!("fn={fn_name}")]),
        ),
        (&target, 1, ip.to_vec()),
    ])
}

#[test]
fn mdns_only_chromecast_uses_txt_friendly_name() {
    // C02: a TV that never answers SSDP DIAL still announces
    // _googlecast._tcp; its name is the TXT fn, not an address.
    let pkt = googlecast_packet(
        "LivingRoom._googlecast._tcp.local",
        "Living Room TV",
        [192, 168, 1, 50],
    );
    let records = blindrss_core::cast::parse_mdns_records(&pkt);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].txt_get("md"), Some("Chromecast Ultra"));
    let devices = blindrss_core::cast::chromecast_devices_from_records(&records);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Living Room TV");
    assert_eq!(devices[0].host, "192.168.1.50");
    assert_eq!(devices[0].port, 8009);
    assert_eq!(devices[0].model, "Chromecast Ultra");
    assert_eq!(devices[0].kind, "chromecast");
    // The triple parser still sees the same instance.
    let triples = blindrss_core::cast::parse_mdns_response(&pkt);
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0].1, "192.168.1.50");
}

#[test]
fn mdns_and_ssdp_merge_preserves_friendly_names() {
    // C02 merge: the SSDP entry had only its host as a name;
    // the mDNS TXT name replaces it, the model fills in, and an
    // mDNS-only device joins the list.
    let ssdp = blindrss_core::cast::parse_ssdp_packet(
        "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.50:8008/ssdp/device-desc.xml\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n",
    )
    .unwrap();
    assert_eq!(ssdp.name, "192.168.1.50"); // fallback name
    let pkt = googlecast_packet(
        "LivingRoom._googlecast._tcp.local",
        "Living Room TV",
        [192, 168, 1, 50],
    );
    let pkt2 = googlecast_packet(
        "Bedroom._googlecast._tcp.local",
        "Bedroom Speaker",
        [192, 168, 1, 51],
    );
    let mut records = blindrss_core::cast::parse_mdns_records(&pkt);
    records.extend(blindrss_core::cast::parse_mdns_records(&pkt2));
    let mdns = blindrss_core::cast::chromecast_devices_from_records(&records);
    let merged = blindrss_core::cast::merge_chromecast_discovery(vec![ssdp], mdns);
    assert_eq!(merged.len(), 2);
    let living = merged.iter().find(|d| d.host == "192.168.1.50").unwrap();
    assert_eq!(living.name, "Living Room TV");
    assert_eq!(living.model, "Chromecast Ultra");
    assert!(merged.iter().any(|d| d.name == "Bedroom Speaker"));
}

#[test]
fn googlecast_name_collision_is_disambiguated_by_host() {
    let pkt = googlecast_packet("One._googlecast._tcp.local", "TV", [192, 168, 1, 60]);
    let pkt2 = googlecast_packet("Two._googlecast._tcp.local", "TV", [192, 168, 1, 61]);
    let mut records = blindrss_core::cast::parse_mdns_records(&pkt);
    records.extend(blindrss_core::cast::parse_mdns_records(&pkt2));
    let devices = blindrss_core::cast::chromecast_devices_from_records(&records);
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "TV");
    assert_eq!(devices[1].name, "TV (192.168.1.61)");
}

fn raop_packet(instance: &str, txt: &[&str], ip: [u8; 4]) -> Vec<u8> {
    mdns_packet(&[
        ("_raop._tcp.local", 12, ptr_rdata(instance)),
        (instance, 33, srv_rdata(5000, "receiver.local")),
        (instance, 16, txt_rdata(txt)),
        ("receiver.local", 1, ip.to_vec()),
    ])
}

#[test]
fn airplay_filter_keeps_classic_raop_only() {
    // C05: classic RAOP (no pairing) is kept; a pairing-required
    // receiver (TXT pw=true) and a Sonos announcement are not
    // AirPlay targets.
    let classic = raop_packet(
        "AABBCC@Airport Express",
        &["am=AirPort10,115", "et=0,1"],
        [192, 168, 1, 70],
    );
    let pairing = raop_packet(
        "DDEEFF@Apple TV",
        &["am=AppleTV6,2", "pw=true"],
        [192, 168, 1, 71],
    );
    let sonos = raop_packet(
        "RINCON_001@Sonos One",
        &["am=SonosOne", "et=0,1"],
        [192, 168, 1, 72],
    );
    let mut records = blindrss_core::cast::parse_mdns_records(&classic);
    records.extend(blindrss_core::cast::parse_mdns_records(&pairing));
    records.extend(blindrss_core::cast::parse_mdns_records(&sonos));
    assert_eq!(records.len(), 3);
    let devices = blindrss_core::cast::airplay_devices_from_records(&records);
    assert_eq!(devices.len(), 1, "{devices:?}");
    assert_eq!(devices[0].name, "Airport Express");
    assert_eq!(devices[0].host, "192.168.1.70");
    assert_eq!(devices[0].port, 5000);
}

#[test]
fn sonos_detection_matches_caster() {
    assert!(blindrss_core::cast::looks_like_sonos("Kitchen", "Sonos"));
    assert!(blindrss_core::cast::looks_like_sonos("RINCON_001", ""));
    assert!(blindrss_core::cast::looks_like_sonos("Amp", "Sonos, Inc."));
    assert!(!blindrss_core::cast::looks_like_sonos(
        "Living Room TV",
        "LG"
    ));
}

#[test]
fn upnp_description_full_parse_reads_manufacturer_and_services() {
    // C04/C06 plumbing: the manufacturer is what lets discovery
    // filter Sonos out of the generic list; the topology URL is
    // where group state lives.
    let xml = "<root><device>\
        <friendlyName>RX-V485</friendlyName>\
        <manufacturer>Yamaha Corporation</manufacturer>\
        <modelName>RX-V485</modelName>\
        <serviceList>\
        <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>\
        <controlURL>/AVTransport/ctrl</controlURL></service>\
        <service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>\
        <controlURL>/RenderingControl/ctrl</controlURL></service>\
        </serviceList></device></root>";
    let desc =
        blindrss_core::cast::parse_upnp_description_full(xml, "http://192.168.1.80:49152/desc.xml")
            .unwrap();
    assert_eq!(desc.manufacturer, "Yamaha Corporation");
    assert_eq!(
        desc.av_transport_url,
        "http://192.168.1.80:49152/AVTransport/ctrl"
    );
    assert_eq!(
        desc.rendering_control_url,
        "http://192.168.1.80:49152/RenderingControl/ctrl"
    );
    assert!(desc.zone_group_topology_url.is_empty());
    assert!(!blindrss_core::cast::looks_like_sonos(
        &desc.name,
        &desc.manufacturer
    ));

    let sonos_xml = "<root><device>\
        <friendlyName>Kitchen</friendlyName>\
        <manufacturer>Sonos, Inc.</manufacturer>\
        <serviceList>\
        <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>\
        <controlURL>/MediaRenderer/AVTransport/Control</controlURL></service>\
        <service><serviceType>urn:schemas-upnp-org:service:ZoneGroupTopology:1</serviceType>\
        <controlURL>/ZoneGroupTopology/Control</controlURL></service>\
        </serviceList></device></root>";
    let desc = blindrss_core::cast::parse_upnp_description_full(
        sonos_xml,
        "http://192.168.1.90:1400/xml/device_description.xml",
    )
    .unwrap();
    assert_eq!(
        desc.zone_group_topology_url,
        "http://192.168.1.90:1400/ZoneGroupTopology/Control"
    );
    assert!(blindrss_core::cast::looks_like_sonos(
        &desc.name,
        &desc.manufacturer
    ));
}
