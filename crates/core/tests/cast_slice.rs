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
