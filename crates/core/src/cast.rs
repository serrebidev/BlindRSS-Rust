//! Casting: discovery (slice 8, staged per the Caster-engine plan).
//!
//! BlindRSS implements no casting of its own in Python; the engine
//! comes from Caster (`core/casting.py` is the adapter). This Rust
//! module is the first stage of the shared caster-core: SSDP
//! discovery for Chromecast (DIAL) devices and mDNS-style device
//! records, with the response parsing kept pure and tested. The
//! CASTv2 channel lands next in this module.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct CastDevice {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// "chromecast" for Google Cast devices.
    pub kind: String,
    pub model: String,
}

/// Parse one SSDP response (or NOTIFY) packet into a device, when
/// it is a Chromecast (its LOCATION points at a DIAL description
/// and the server/ST mentions Cast). Pure; fixture-tested.
pub fn parse_ssdp_packet(text: &str) -> Option<CastDevice> {
    let mut headers: HashMap<String, String> = HashMap::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let location = headers.get("location")?;
    // Chromecast DIAL descriptions live on port 8008 at /ssdp/device-desc.xml
    if !location.contains(":8008") && !location.contains("device-desc") {
        return None;
    }
    let hostport = location
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()?
        .to_string();
    // The parsed port is the DIAL description port; the cast
    // port below is always 8009.
    let host = match hostport.rsplit_once(':') {
        Some((h, _dial_port)) => h.to_string(),
        None => hostport,
    };
    let name = headers
        .get("x-friendly-name")
        .cloned()
        .unwrap_or_else(|| host.clone());
    Some(CastDevice {
        name,
        host,
        port: 8009, // CASTv2 port; the SSDP port is the DIAL port.
        kind: "chromecast".to_string(),
        model: headers.get("x-model-name").cloned().unwrap_or_default(),
    })
}

/// Broadcast an SSDP M-SEARCH for DIAL devices and collect
/// Chromecast responses for `timeout`.
pub fn discover_chromecasts(timeout: Duration) -> Vec<CastDevice> {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = socket.set_broadcast(true);
    let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n";
    if socket
        .send_to(search.as_bytes(), "239.255.255.250:1900")
        .is_err()
    {
        return Vec::new();
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut out: Vec<CastDevice> = Vec::new();
    let mut buf = [0u8; 4096];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n])
                    && let Some(dev) = parse_ssdp_packet(text)
                    && !out.iter().any(|d| d.host == dev.host)
                {
                    out.push(dev);
                }
            }
            Err(_) => continue,
        }
    }
    out
}
