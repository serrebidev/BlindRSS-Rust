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
    /// "chromecast" | "dlna" | "sonos" | "roku" | "kodi" |
    /// "airplay" (RAOP; host+port are the RTSP endpoint).
    pub kind: String,
    pub model: String,
    /// Protocol endpoint when discovery resolved one (UPnP
    /// AVTransport control URL, Roku base URL); empty when
    /// host+port are enough.
    pub endpoint: String,
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
        endpoint: String::new(),
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

// ---------------------------------------------------------------------------
// The other Caster protocols (caster_devices.py / caster_extras.py):
// DLNA/UPnP MediaRenderers, Sonos zone players, Roku ECP, and
// Kodi (mDNS). Discovery shares the SSDP machinery above; Kodi
// answers mDNS, not SSDP.
// ---------------------------------------------------------------------------

/// Fetch a UPnP device description and pull the friendly name
/// plus the AVTransport control URL (resolved against the
/// description's base). Hand-parsed: the documents are small
/// and regular.
pub fn parse_upnp_description(xml: &str, location: &str) -> Option<(String, String, String)> {
    let name = xml_tag(xml, "friendlyName")?;
    // Find the AVTransport service block and its controlURL.
    let mut control: Option<String> = None;
    let mut rest = xml;
    while let Some(pos) = rest.find("<service>") {
        let block_end = rest[pos..].find("</service>").map(|e| pos + e)?;
        let block = &rest[pos..block_end];
        if block.contains("AVTransport") {
            control = xml_tag(block, "controlURL");
            break;
        }
        rest = &rest[block_end..];
    }
    let control = control?;
    let base = location
        .rsplit_once('/')
        .map(|(b, _)| b.to_string())
        .unwrap_or_default();
    let resolved = if control.starts_with("http") {
        control
    } else if let Some(stripped) = control.strip_prefix('/') {
        // Root-relative: scheme + authority from the location.
        let authority = location
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("");
        let scheme = location.split("://").next().unwrap_or("http");
        format!("{scheme}://{authority}/{stripped}")
    } else {
        format!("{base}/{control}")
    };
    let model = xml_tag(xml, "modelName").unwrap_or_default();
    Some((name, resolved, model))
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// One SSDP sweep for several search targets; returns raw
/// (ST, LOCATION, headers) hits for the protocol parsers.
fn ssdp_sweep(targets: &[&str], timeout: Duration) -> Vec<(String, String)> {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = socket.set_broadcast(true);
    for st in targets {
        let search = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
        );
        let _ = socket.send_to(search.as_bytes(), "239.255.255.250:1900");
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut out: Vec<(String, String)> = Vec::new();
    let mut buf = [0u8; 8192];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    out.push((text.to_string(), String::new()));
                }
            }
            Err(_) => continue,
        }
    }
    out
}

fn ssdp_headers(text: &str) -> HashMap<String, String> {
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
    headers
}

fn http_get_text(url: &str) -> Option<String> {
    let mut resp = crate::http::agent().get(url).call().ok()?;
    resp.body_mut().read_to_string().ok()
}

/// Discover every supported cast device kind on the LAN.
/// Chromecasts come from the DIAL sweep above; this adds
/// DLNA renderers, Sonos players, Roku (ECP), and Kodi
/// (mDNS + configured hosts).
pub fn discover_all(timeout: Duration) -> Vec<CastDevice> {
    let mut out = discover_chromecasts(timeout);
    let hits = ssdp_sweep(
        &[
            "urn:schemas-upnp-org:device:MediaRenderer:1",
            "urn:schemas-upnp-org:device:ZonePlayer:1",
            "roku:ecp",
        ],
        timeout,
    );
    let mut seen_locations: Vec<String> = Vec::new();
    for (packet, _) in &hits {
        let headers = ssdp_headers(packet);
        let Some(location) = headers.get("location") else {
            continue;
        };
        if seen_locations.contains(location) {
            continue;
        }
        seen_locations.push(location.clone());
        let st = headers.get("st").cloned().unwrap_or_default();
        let host = location
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        if st.contains("ZonePlayer") {
            if let Some(xml) = http_get_text(location)
                && let Some((name, control, model)) = parse_upnp_description(&xml, location)
            {
                out.push(CastDevice {
                    name,
                    host: host.clone(),
                    port: 1400,
                    kind: "sonos".to_string(),
                    model,
                    endpoint: control,
                });
            }
        } else if st.contains("MediaRenderer") {
            if let Some(xml) = http_get_text(location)
                && let Some((name, control, model)) = parse_upnp_description(&xml, location)
            {
                // A Sonos also answers MediaRenderer; the
                // ZonePlayer hit wins by dedupe on host.
                if !out.iter().any(|d| d.host == host) {
                    out.push(CastDevice {
                        name,
                        host: host.clone(),
                        port: 0,
                        kind: "dlna".to_string(),
                        model,
                        endpoint: control,
                    });
                }
            }
        } else if st == "roku:ecp" {
            let name = http_get_text(&format!("http://{host}:8060/query/device-info"))
                .and_then(|xml| xml_tag(&xml, "friendly-device-name"))
                .unwrap_or_else(|| format!("Roku {host}"));
            out.push(CastDevice {
                name,
                host: host.clone(),
                port: 8060,
                kind: "roku".to_string(),
                model: String::new(),
                endpoint: format!("http://{host}:8060"),
            });
        }
    }
    out.extend(discover_kodi(timeout));
    out.extend(discover_airplay(timeout));
    out
}

/// AirPlay (RAOP) discovery: mDNS for `_raop._tcp`. Instance
/// names are "{device-id}@{display name}". The port is the
/// RAOP RTSP port (5000 on classic AirPort, dynamic on newer
/// receivers).
pub fn discover_airplay(timeout: Duration) -> Vec<CastDevice> {
    let mut out = Vec::new();
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return out;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let query = mdns_ptr_query("_raop._tcp.local");
    let _ = socket.send_to(&query, "224.0.0.251:5353");
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 8192];
    let mut names: Vec<(String, String, u16)> = Vec::new();
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                for (instance, host, port) in parse_mdns_response(&buf[..n]) {
                    if !names.iter().any(|(i, _, _)| *i == instance) {
                        names.push((instance, host, port));
                    }
                }
            }
            Err(_) => continue,
        }
    }
    for (instance, host, port) in names {
        let label = instance.split("._").next().unwrap_or("AirPlay");
        let name = label
            .split_once('@')
            .map(|(_, n)| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| label.to_string());
        out.push(CastDevice {
            name,
            host,
            port: if port == 0 { 5000 } else { port },
            kind: "airplay".to_string(),
            model: String::new(),
            endpoint: String::new(),
        });
    }
    out
}

/// Kodi discovery: mDNS for the JSON-RPC service plus the
/// configured hosts (config cast_kodi_hosts, not read here —
/// the app appends those itself).
pub fn discover_kodi(timeout: Duration) -> Vec<CastDevice> {
    let mut out = Vec::new();
    // mDNS PTR query for _kodi-jsonrpc._tcp and the older
    // _xbmc-jsonrpc._tcp, then SRV/A follow-ups are folded
    // into the same response set by responders.
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return out;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    for service in ["_kodi-jsonrpc._tcp.local", "_xbmc-jsonrpc._tcp.local"] {
        let query = mdns_ptr_query(service);
        let _ = socket.send_to(&query, "224.0.0.251:5353");
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 8192];
    let mut names: Vec<(String, String, u16)> = Vec::new(); // (instance, host, port)
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                for (instance, host, port) in parse_mdns_response(&buf[..n]) {
                    if !names.iter().any(|(i, _, _)| *i == instance) {
                        names.push((instance, host, port));
                    }
                }
            }
            Err(_) => continue,
        }
    }
    for (instance, host, port) in names {
        let label = instance.split("._").next().unwrap_or("Kodi").to_string();
        out.push(CastDevice {
            name: if label.is_empty() {
                "Kodi".to_string()
            } else {
                label
            },
            host,
            port: if port == 0 { 8080 } else { port },
            kind: "kodi".to_string(),
            model: String::new(),
            endpoint: String::new(),
        });
    }
    out
}

fn mdns_name(name: &str, out: &mut Vec<u8>) {
    for part in name.split('.') {
        out.push(part.len() as u8);
        out.extend_from_slice(part.as_bytes());
    }
    out.push(0);
}

/// Build an mDNS PTR query packet for a service name.
pub fn mdns_ptr_query(service: &str) -> Vec<u8> {
    let mut pkt = vec![0u8; 12]; // id=0, flags=0, qd=1
    pkt[4] = 0;
    pkt[5] = 1;
    mdns_name(service, &mut pkt);
    pkt.extend_from_slice(&12u16.to_be_bytes()); // PTR
    pkt.extend_from_slice(&1u16.to_be_bytes()); // IN
    pkt
}

/// Parse an mDNS response into (instance, host, port) triples
/// by joining PTR -> SRV -> A records. Tolerant by design:
/// anything malformed is skipped.
pub fn parse_mdns_response(pkt: &[u8]) -> Vec<(String, String, u16)> {
    fn read_name(pkt: &[u8], mut off: usize) -> Option<(String, usize)> {
        let mut labels = Vec::new();
        let mut jumped = None;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 30 || off >= pkt.len() {
                return None;
            }
            let len = pkt[off];
            if len == 0 {
                return Some((labels.join("."), jumped.unwrap_or(off + 1)));
            }
            if len & 0xC0 == 0xC0 {
                if off + 1 >= pkt.len() {
                    return None;
                }
                let ptr = (((len & 0x3F) as usize) << 8) | pkt[off + 1] as usize;
                if jumped.is_none() {
                    jumped = Some(off + 2);
                }
                off = ptr;
                continue;
            }
            let start = off + 1;
            let end = start + len as usize;
            if end > pkt.len() {
                return None;
            }
            labels.push(String::from_utf8_lossy(&pkt[start..end]).to_string());
            off = end;
        }
    }
    fn u16_at(pkt: &[u8], off: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*pkt.get(off)?, *pkt.get(off + 1)?]))
    }
    if pkt.len() < 12 {
        return Vec::new();
    }
    let qd = u16_at(pkt, 4).unwrap_or(0) as usize;
    let an = u16_at(pkt, 6).unwrap_or(0) as usize;
    let ns = u16_at(pkt, 8).unwrap_or(0) as usize;
    let ar = u16_at(pkt, 10).unwrap_or(0) as usize;
    let mut off = 12usize;
    for _ in 0..qd {
        match read_name(pkt, off) {
            Some((_, next)) => off = next + 4,
            None => return Vec::new(),
        }
    }
    let mut ptrs: Vec<String> = Vec::new();
    let mut srvs: Vec<(String, String, u16)> = Vec::new();
    let mut addrs: Vec<(String, String)> = Vec::new();
    for _ in 0..(an + ns + ar) {
        let Some((name, next)) = read_name(pkt, off) else {
            break;
        };
        off = next;
        let (Some(rtype), Some(rdlen)) = (u16_at(pkt, off), u16_at(pkt, off + 8)) else {
            break;
        };
        let rdata = off + 10;
        match rtype {
            12 => {
                // PTR
                if let Some((target, _)) = read_name(pkt, rdata) {
                    ptrs.push(target);
                }
            }
            33 => {
                // SRV: priority/weight/port + target
                if let Some(port) = u16_at(pkt, rdata + 4)
                    && let Some((target, _)) = read_name(pkt, rdata + 6) {
                        srvs.push((name.clone(), target, port));
                    }
            }
            1
                // A
                if rdlen == 4 && rdata + 4 <= pkt.len() => {
                    addrs.push((
                        name.clone(),
                        format!(
                            "{}.{}.{}.{}",
                            pkt[rdata],
                            pkt[rdata + 1],
                            pkt[rdata + 2],
                            pkt[rdata + 3]
                        ),
                    ));
                }
            _ => {}
        }
        off = rdata + rdlen as usize;
    }
    let mut out = Vec::new();
    for instance in ptrs {
        if let Some((_, target, port)) = srvs.iter().find(|(n, _, _)| *n == instance) {
            let host = addrs
                .iter()
                .find(|(n, _)| n == target)
                .map(|(_, ip)| ip.clone())
                .unwrap_or_else(|| target.clone());
            out.push((instance, host, *port));
        }
    }
    out
}
